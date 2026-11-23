//! The finishing touches on a take that `mp4mux` cannot make itself: dropping the room it
//! reserved for the index, and marking the audio tracks as alternates.
//!
//! The reserved room stays in a finished file as padding, the same tens of megabytes whatever the
//! take's length. A take whose padding is a noticeable part of it is copied once into a file
//! without it, and the copy takes the take's name in one rename, so a crash at any moment leaves
//! one whole file under that name. The kernel copies the samples, and Btrfs and XFS share them
//! instead, because the copy keeps their place within a block. A long take keeps its padding,
//! where copying it would cost more than the room it frees.
//!
//! The audio tracks become one alternate group with only the first (the mix) enabled, as the
//! macOS writer leaves them, so a player that plays every enabled track (QuickTime) does not play
//! the mix, the microphone and the system audio at once.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

use crate::files;

/// The largest index read into memory: a four-hour take's is a few megabytes.
const MAX_MOOV_BYTES: u64 = 256 * 1024 * 1024;
/// Padding below this is left where it is.
const MIN_PADDING: u64 = 1024 * 1024;
/// A take is copied without its padding when the padding is at least this share of it: 2 %,
/// which is every take under about three gigabytes with four hours reserved.
const PADDING_SHARE: u64 = 50;
/// The block a copy keeps the samples aligned to, so a file system that shares blocks can.
const BLOCK: u64 = 4096;

/// A box inside `data`: its type and where its payload lies.
struct Child {
    kind: [u8; 4],
    start: usize,
    end: usize,
}

fn children(data: &[u8], from: usize, to: usize) -> Vec<Child> {
    let mut boxes = Vec::new();
    let mut at = from;
    while at + 8 <= to {
        let size = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        let (header, size) = match size {
            1 if at + 16 <= to => {
                let mut wide = [0u8; 8];
                wide.copy_from_slice(&data[at + 8..at + 16]);
                (16, u64::from_be_bytes(wide) as usize)
            }
            0 => (8, to - at),
            n => (8, n),
        };
        if size < header || at + size > to {
            break;
        }
        boxes.push(Child { kind, start: at + header, end: at + size });
        at += size;
    }
    boxes
}

/// A top-level box of the file: its type, offset and whole size.
struct TopBox {
    kind: [u8; 4],
    offset: u64,
    size: u64,
}

/// The top-level boxes of `file`, or `None` when they do not add up to the file.
fn top_level(file: &mut File) -> io::Result<Option<Vec<TopBox>>> {
    let length = file.metadata()?.len();
    let mut boxes = Vec::new();
    let mut at = 0u64;
    while at + 8 <= length {
        file.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 16];
        let wanted = (length - at).min(16) as usize;
        file.read_exact(&mut header[..wanted])?;
        let small = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let size = match small {
            1 if wanted == 16 => {
                let mut wide = [0u8; 8];
                wide.copy_from_slice(&header[8..16]);
                u64::from_be_bytes(wide)
            }
            0 => length - at,
            n => n,
        };
        if size < 8 || at + size > length {
            return Ok(None);
        }
        boxes.push(TopBox { kind: [header[4], header[5], header[6], header[7]], offset: at, size });
        at += size;
    }
    Ok((at == length).then_some(boxes))
}

/// The child boxes of `parent` (inside `moov`) by path, each level matching one type.
fn descend(moov: &[u8], from: usize, to: usize, path: &[&[u8; 4]]) -> Vec<Child> {
    let Some((first, rest)) = path.split_first() else { return vec![] };
    let found = children(moov, from, to).into_iter().filter(|c| &c.kind == *first);
    if rest.is_empty() {
        return found.collect();
    }
    found.flat_map(|c| descend(moov, c.start, c.end, rest)).collect()
}

fn moov_header(moov: &[u8]) -> usize {
    if moov.len() >= 16 && moov[..4] == [0, 0, 0, 1] {
        16
    } else {
        8
    }
}

/// Puts the audio tracks in one alternate group with only the first enabled; says whether it
/// changed anything.
fn mark_alternate_audio(moov: &mut [u8]) -> bool {
    let header = moov_header(moov);
    let mut audio = 0;
    let mut changes: Vec<(usize, u8)> = Vec::new();
    for trak in children(moov, header, moov.len()).into_iter().filter(|c| &c.kind == b"trak") {
        let inside = children(moov, trak.start, trak.end);
        let is_audio = descend(moov, trak.start, trak.end, &[b"mdia", b"hdlr"])
            .iter()
            .any(|c| c.end >= c.start + 12 && &moov[c.start + 8..c.start + 12] == b"soun");
        let Some(tkhd) = inside.iter().find(|c| &c.kind == b"tkhd") else { continue };
        if !is_audio || tkhd.end <= tkhd.start {
            continue;
        }
        // The alternate group sits after the times, the id, the duration and the layer: 34 bytes
        // into a version 0 header, 46 into a version 1 one.
        let group = tkhd.start + if moov[tkhd.start] == 1 { 46 } else { 34 };
        if group + 2 > tkhd.end {
            continue;
        }
        changes.push((group, 0));
        changes.push((group + 1, 1));
        if audio > 0 {
            let flags = tkhd.start + 3;
            changes.push((flags, moov[flags] & !1));
        }
        audio += 1;
    }
    if audio < 2 {
        return false;
    }
    for (at, byte) in changes {
        moov[at] = byte;
    }
    true
}

/// Moves every chunk offset of the index `by` bytes towards the start. Refuses (and leaves
/// `moov` as it was) when an offset would point in front of `floor`, outside the samples.
fn shift_chunks(moov: &mut [u8], by: u64, floor: u64) -> bool {
    let header = moov_header(moov);
    let stbls = descend(moov, header, moov.len(), &[b"trak", b"mdia", b"minf", b"stbl"]);
    let mut tables = Vec::new();
    for stbl in &stbls {
        for table in children(moov, stbl.start, stbl.end) {
            let width = match &table.kind {
                b"stco" => 4,
                b"co64" => 8,
                _ => continue,
            };
            if table.end < table.start + 8 {
                return false;
            }
            let count = u32::from_be_bytes([
                moov[table.start + 4],
                moov[table.start + 5],
                moov[table.start + 6],
                moov[table.start + 7],
            ]) as usize;
            let first = table.start + 8;
            if first + count * width > table.end {
                return false;
            }
            tables.push((first, count, width));
        }
    }
    if tables.len() != stbls.len() {
        return false;
    }
    let read = |moov: &[u8], at: usize, width: usize| -> u64 {
        moov[at..at + width].iter().fold(0u64, |v, b| v << 8 | *b as u64)
    };
    let fits =
        tables.iter().all(|&(first, count, width)| (0..count).all(|i| read(moov, first + i * width, width) >= floor));
    if !fits {
        return false;
    }
    for (first, count, width) in tables {
        for i in 0..count {
            let at = first + i * width;
            let moved = read(moov, at, width) - by;
            let bytes = moved.to_be_bytes();
            moov[at..at + width].copy_from_slice(&bytes[8 - width..]);
        }
    }
    true
}

/// What a finished take holds in front of its samples.
struct Layout {
    ftyp: TopBox,
    /// Where the samples (`mdat`) start; everything from here on is copied as it is.
    samples: u64,
}

/// The layout `mp4mux` leaves: `ftyp`, the index and its padding, then one `mdat` that runs to
/// the end. Anything else is left alone.
fn layout(boxes: Vec<TopBox>) -> Option<Layout> {
    let mut boxes = boxes.into_iter();
    let ftyp = boxes.next().filter(|b| &b.kind == b"ftyp")?;
    let mut moov = false;
    for b in boxes.by_ref() {
        match &b.kind {
            b"moov" if !moov => moov = true,
            b"free" | b"skip" => {}
            b"mdat" => {
                let rest: Vec<TopBox> = boxes.collect();
                if !rest.iter().all(|r| matches!(&r.kind, b"free" | b"skip")) {
                    return None;
                }
                return moov.then_some(Layout { ftyp, samples: b.offset });
            }
            _ => return None,
        }
    }
    None
}

/// Writes `ftyp`, the shifted index and the samples of `source` into `part`, whose samples start
/// at `start`.
fn write_compact(source: &mut File, part: &mut File, layout: &Layout, moov: &[u8], start: u64) -> io::Result<()> {
    let mut ftyp = vec![0u8; layout.ftyp.size as usize];
    source.read_exact_at(&mut ftyp, layout.ftyp.offset)?;
    part.write_all(&ftyp)?;
    part.write_all(moov)?;
    let gap = start - layout.ftyp.size - moov.len() as u64;
    if gap > 0 {
        let mut free = vec![0u8; gap as usize];
        free[..4].copy_from_slice(&(gap as u32).to_be_bytes());
        free[4..8].copy_from_slice(b"free");
        part.write_all(&free)?;
    }
    source.seek(SeekFrom::Start(layout.samples))?;
    let length = source.metadata()?.len() - layout.samples;
    // `io::copy` between two files is the kernel's `copy_file_range`, which shares the blocks
    // where the file system can.
    let copied = io::copy(&mut Read::take(&mut *source, length), part)?;
    if copied != length {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the samples were cut short while they were copied"));
    }
    part.sync_all()
}

/// Copies the take at `path` without its padding, and gives the copy the take's name.
fn compact(path: &Path, source: &mut File, layout: &Layout, moov: &[u8]) -> io::Result<bool> {
    let front = layout.ftyp.size + moov.len() as u64;
    // The samples keep their place within a block; a `free` box fills the difference.
    let mut start = front + (layout.samples - front) % BLOCK;
    if start > front && start - front < 8 {
        start += BLOCK;
    }
    if start >= layout.samples {
        return Ok(false);
    }
    let length = source.metadata()?.len();
    let free = path.parent().and_then(|dir| files::free_bytes(dir).ok()).unwrap_or(0);
    if free < length.saturating_add(files::LOW_DISK_BYTES) {
        return Ok(false);
    }
    let mut shifted = moov.to_vec();
    if !shift_chunks(&mut shifted, layout.samples - start, layout.samples) {
        return Ok(false);
    }
    let part_path = files::part_path(path, "compact");
    let mut part = OpenOptions::new().write(true).create_new(true).open(&part_path)?;
    let written = write_compact(source, &mut part, layout, &shifted, start).and_then(|()| {
        // The name must still be this take's: the rename replaces whatever file has it.
        let (now, ours) = (std::fs::metadata(path)?, source.metadata()?);
        if (now.dev(), now.ino()) != (ours.dev(), ours.ino()) {
            return Err(io::Error::other("the take was moved while it was finished"));
        }
        std::fs::rename(&part_path, path)
    });
    drop(part);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part_path);
        return Err(e);
    }
    if let Some(dir) = path.parent() {
        let _ = File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(true)
}

fn read_u32(data: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?) as u64)
}

/// The average frame rate of the file's video track: its frames over the time they cover, from
/// the sample durations, as AVFoundation's nominal rate is. The demuxer gives none for a take
/// whose frames came at uneven times.
pub(super) fn video_rate(path: &Path) -> Option<f64> {
    let mut file = File::open(path).ok()?;
    let boxes = top_level(&mut file).ok()??;
    let moov = boxes.iter().find(|b| &b.kind == b"moov").filter(|b| b.size <= MAX_MOOV_BYTES)?;
    let mut data = vec![0u8; moov.size as usize];
    file.read_exact_at(&mut data, moov.offset).ok()?;
    let header = moov_header(&data);
    let mdia = descend(&data, header, data.len(), &[b"trak", b"mdia"]).into_iter().find(|mdia| {
        descend(&data, mdia.start, mdia.end, &[b"hdlr"])
            .iter()
            .any(|c| c.end >= c.start + 12 && &data[c.start + 8..c.start + 12] == b"vide")
    })?;
    let mdhd = descend(&data, mdia.start, mdia.end, &[b"mdhd"]).into_iter().next()?;
    let timescale = read_u32(&data, mdhd.start + if *data.get(mdhd.start)? == 1 { 20 } else { 12 })?;
    let stts = descend(&data, mdia.start, mdia.end, &[b"minf", b"stbl", b"stts"]).into_iter().next()?;
    let entries = read_u32(&data, stts.start + 4)? as usize;
    let (mut frames, mut ticks) = (0u64, 0u64);
    for i in 0..entries {
        let at = stts.start + 8 + i * 8;
        if at + 8 > stts.end {
            return None;
        }
        let (count, delta) = (read_u32(&data, at)?, read_u32(&data, at + 4)?);
        frames += count;
        ticks += count * delta;
    }
    (ticks > 0 && timescale > 0).then(|| frames as f64 * timescale as f64 / ticks as f64)
}

/// Finishes the take at `path`: drops the padding when it is worth a copy, and marks the audio
/// tracks as alternates when there are several. A file it cannot touch stays as it is and still
/// plays.
pub(super) fn finish(path: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let Some(boxes) = top_level(&mut file)? else { return Ok(()) };
    let length = file.metadata()?.len();
    let Some(moov) = boxes.iter().find(|b| &b.kind == b"moov") else { return Ok(()) };
    if moov.size > MAX_MOOV_BYTES {
        return Ok(());
    }
    let (offset, size) = (moov.offset, moov.size);
    let mut index = vec![0u8; size as usize];
    file.read_exact_at(&mut index, offset)?;
    let marked = mark_alternate_audio(&mut index);
    if let Some(layout) = layout(boxes) {
        let padding = layout.samples - layout.ftyp.size - size;
        if padding >= MIN_PADDING && padding.saturating_mul(PADDING_SHARE) >= length {
            // A copy that fails (a full disk, a file system that refuses) leaves the take with
            // its padding, which still plays.
            if let Ok(true) = compact(path, &mut file, &layout, &index) {
                return Ok(());
            }
        }
    }
    if marked {
        file.write_all_at(&index, offset)?;
        file.sync_all()?;
    }
    Ok(())
}
