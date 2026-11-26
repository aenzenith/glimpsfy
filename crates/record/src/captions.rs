//! Speech as subtitles: SRT and WebVTT text, and the `tx3g` subtitle track an MP4 export carries,
//! which players list among their subtitles and switch on and off.
//!
//! The track is added to a finished file without touching its samples. The file's index (`moov`)
//! grows by one track, so every chunk offset that points behind the index moves by the growth. An
//! index in front of the media stays in front, so the file still starts playing before it is
//! fully read; that file is copied once into a sibling, which takes its name in one rename. An
//! index at the end is replaced where it lies, so its file is not copied. Only the index is held
//! in memory; the media is streamed.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::{files, RecordError, Result};

/// One subtitle, in milliseconds of the exported file. `text` may hold '\n' between its (at most
/// two) lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caption {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// The largest index read into memory: a four-hour take's is a few megabytes.
const MAX_MOOV_BYTES: u64 = 256 * 1024 * 1024;
/// The text track counts in milliseconds, the unit the captions come in.
const TEXT_TIMESCALE: u64 = 1000;
/// A large buffer keeps a copy of many gigabytes to few system calls.
const COPY_BUFFER: usize = 1024 * 1024;

/// A caption as it is written: in time order, inside the file, with clean lines.
struct Cue {
    start: u64,
    end: u64,
    text: String,
}

/// The captions sorted by time, each starting no earlier than the one before ends and ending by
/// `limit_ms`. Blank lines and carriage returns are dropped, because a blank line ends an SRT or
/// VTT cue early; a caption left without text or time is dropped as well.
fn cues(captions: &[Caption], limit_ms: u64) -> Vec<Cue> {
    let mut sorted: Vec<&Caption> = captions.iter().collect();
    sorted.sort_by_key(|c| (c.start_ms, c.end_ms));
    let mut floor = 0;
    let mut cues = Vec::new();
    for caption in sorted {
        let lines: Vec<&str> = caption.text.split(['\n', '\r']).map(str::trim).filter(|l| !l.is_empty()).collect();
        let start = caption.start_ms.max(floor);
        let end = caption.end_ms.min(limit_ms);
        if end <= start || lines.is_empty() {
            continue;
        }
        floor = end;
        cues.push(Cue { start, end, text: lines.join("\n") });
    }
    cues
}

fn stamp(ms: u64, separator: char) -> String {
    format!("{:02}:{:02}:{:02}{separator}{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// The captions as an SRT file.
pub fn srt(captions: &[Caption]) -> String {
    let mut out = String::new();
    for (n, cue) in cues(captions, u64::MAX).iter().enumerate() {
        // SRT has no escapes, and a lenient reader takes a line with an arrow for a timing line.
        let mut text = cue.text.clone();
        while text.contains("-->") {
            text = text.replace("-->", "->");
        }
        let _ = write!(out, "{}\n{} --> {}\n{text}\n\n", n + 1, stamp(cue.start, ','), stamp(cue.end, ','));
    }
    out
}

/// The captions as a WebVTT file.
pub fn vtt(captions: &[Caption]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for cue in cues(captions, u64::MAX) {
        // Escaping `>` as well turns every "-->", which a cue must not hold, into "--&gt;".
        let text = cue.text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let _ = write!(out, "{} --> {}\n{text}\n\n", stamp(cue.start, '.'), stamp(cue.end, '.'));
    }
    out
}

/// whisper.cpp's languages (`g_lang`) by their ISO 639-1 code, with the ISO 639-2/T code an MP4
/// holds. whisper names three by other codes: Hawaiian has no two-letter code, Javanese keeps the
/// withdrawn "jw", and Cantonese ("yue", ISO 639-3) falls under Chinese, which ISO 639-2 has.
const LANGUAGES: [(&str, &[u8; 3]); 100] = [
    ("en", b"eng"),
    ("zh", b"zho"),
    ("de", b"deu"),
    ("es", b"spa"),
    ("ru", b"rus"),
    ("ko", b"kor"),
    ("fr", b"fra"),
    ("ja", b"jpn"),
    ("pt", b"por"),
    ("tr", b"tur"),
    ("pl", b"pol"),
    ("ca", b"cat"),
    ("nl", b"nld"),
    ("ar", b"ara"),
    ("sv", b"swe"),
    ("it", b"ita"),
    ("id", b"ind"),
    ("hi", b"hin"),
    ("fi", b"fin"),
    ("vi", b"vie"),
    ("he", b"heb"),
    ("uk", b"ukr"),
    ("el", b"ell"),
    ("ms", b"msa"),
    ("cs", b"ces"),
    ("ro", b"ron"),
    ("da", b"dan"),
    ("hu", b"hun"),
    ("ta", b"tam"),
    ("no", b"nor"),
    ("th", b"tha"),
    ("ur", b"urd"),
    ("hr", b"hrv"),
    ("bg", b"bul"),
    ("lt", b"lit"),
    ("la", b"lat"),
    ("mi", b"mri"),
    ("ml", b"mal"),
    ("cy", b"cym"),
    ("sk", b"slk"),
    ("te", b"tel"),
    ("fa", b"fas"),
    ("lv", b"lav"),
    ("bn", b"ben"),
    ("sr", b"srp"),
    ("az", b"aze"),
    ("sl", b"slv"),
    ("kn", b"kan"),
    ("et", b"est"),
    ("mk", b"mkd"),
    ("br", b"bre"),
    ("eu", b"eus"),
    ("is", b"isl"),
    ("hy", b"hye"),
    ("ne", b"nep"),
    ("mn", b"mon"),
    ("bs", b"bos"),
    ("kk", b"kaz"),
    ("sq", b"sqi"),
    ("sw", b"swa"),
    ("gl", b"glg"),
    ("mr", b"mar"),
    ("pa", b"pan"),
    ("si", b"sin"),
    ("km", b"khm"),
    ("sn", b"sna"),
    ("yo", b"yor"),
    ("so", b"som"),
    ("af", b"afr"),
    ("oc", b"oci"),
    ("ka", b"kat"),
    ("be", b"bel"),
    ("tg", b"tgk"),
    ("sd", b"snd"),
    ("gu", b"guj"),
    ("am", b"amh"),
    ("yi", b"yid"),
    ("lo", b"lao"),
    ("uz", b"uzb"),
    ("fo", b"fao"),
    ("ht", b"hat"),
    ("ps", b"pus"),
    ("tk", b"tuk"),
    ("nn", b"nno"),
    ("mt", b"mlt"),
    ("sa", b"san"),
    ("lb", b"ltz"),
    ("my", b"mya"),
    ("bo", b"bod"),
    ("tl", b"tgl"),
    ("mg", b"mlg"),
    ("as", b"asm"),
    ("tt", b"tat"),
    ("haw", b"haw"),
    ("ln", b"lin"),
    ("ha", b"hau"),
    ("ba", b"bak"),
    ("jw", b"jav"),
    ("su", b"sun"),
    ("yue", b"zho"),
];

/// ISO 639-1 code from whisper ("tr", "en", "zh", "und"/unknown) to the ISO 639-2/T code an MP4
/// `mdhd` holds ("tur", "eng", "zho", "und"). A region ("pt-BR") is ignored, a code that is
/// already ISO 639-2/T is kept, and anything else is "und".
pub fn iso639_2(code: &str) -> [u8; 3] {
    let primary = code.split(['-', '_']).next().unwrap_or_default().trim().to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(one, three)| *one == primary || three.as_slice() == primary.as_bytes())
        .map_or(*b"und", |(_, three)| **three)
}

fn broken(why: &str) -> RecordError {
    RecordError::Encoder(format!("the MP4 cannot take a subtitle track: {why}"))
}

fn get(data: &[u8], at: usize, width: usize) -> Result<u64> {
    let bytes = data.get(at..at + width).ok_or_else(|| broken("a box is cut short"))?;
    Ok(bytes.iter().fold(0, |value, b| value << 8 | u64::from(*b)))
}

fn put(data: &mut [u8], at: usize, width: usize, value: u64) {
    data[at..at + width].copy_from_slice(&value.to_be_bytes()[8 - width..]);
}

/// A box inside the index: its type and where its payload lies.
#[derive(Clone, Copy)]
struct Child {
    kind: [u8; 4],
    start: usize,
    end: usize,
}

fn children(data: &[u8], from: usize, to: usize) -> Result<Vec<Child>> {
    let mut boxes = Vec::new();
    let mut at = from;
    // Fewer than eight bytes hold no box: QuickTime ends some lists with a zero word.
    while to - at >= 8 {
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        let (header, size) = match get(data, at, 4)? {
            1 => (16, get(data, at + 8, 8)?),
            0 => (8, (to - at) as u64),
            n => (8, n),
        };
        if size < header as u64 || size > (to - at) as u64 {
            return Err(broken("a box runs past the box that holds it"));
        }
        boxes.push(Child { kind, start: at + header, end: at + size as usize });
        at += size as usize;
    }
    Ok(boxes)
}

fn child(data: &[u8], parent: Child, kind: &[u8; 4]) -> Result<Option<Child>> {
    Ok(children(data, parent.start, parent.end)?.into_iter().find(|c| &c.kind == kind))
}

/// A box at the top of the file.
struct TopBox {
    kind: [u8; 4],
    offset: u64,
    size: u64,
    /// The box said it runs to the end of the file (size 0) instead of giving its size.
    open: bool,
}

fn top_level(file: &mut File, length: u64) -> Result<Vec<TopBox>> {
    let mut boxes = Vec::new();
    let mut at = 0u64;
    while at < length {
        let left = length - at;
        if left < 8 {
            return Err(broken("the file ends inside a box header"));
        }
        let mut header = [0u8; 16];
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut header[..left.min(16) as usize])?;
        let kind = [header[4], header[5], header[6], header[7]];
        let (size, header_len, open) = match u32::from_be_bytes([header[0], header[1], header[2], header[3]]) {
            1 if left >= 16 => (u64::from_be_bytes(header[8..16].try_into().unwrap_or_default()), 16, false),
            0 => (left, 8, true),
            n => (u64::from(n), 8, false),
        };
        if size < header_len || size > left {
            return Err(broken("a box runs past the end of the file"));
        }
        boxes.push(TopBox { kind, offset: at, size, open });
        at += size;
    }
    Ok(boxes)
}

/// A chunk offset table (`stco` or `co64`) inside the index.
struct Table {
    first: usize,
    count: usize,
    width: usize,
}

/// What the new track needs from the index, and where the index changes.
struct Index {
    header: usize,
    /// Where the new track goes: behind the last one, in front of `udta` and the rest.
    insert: usize,
    next_id_at: usize,
    new_id: u32,
    group: i16,
    timescale: u64,
    duration: u64,
    created: u64,
    modified: u64,
    tables: Vec<Table>,
}

fn parse(moov: &[u8]) -> Result<Index> {
    let header = if get(moov, 0, 4)? == 1 { 16 } else { 8 };
    let top = children(moov, header, moov.len())?;
    if top.iter().any(|c| &c.kind == b"mvex") {
        return Err(broken("it is fragmented"));
    }
    let mvhd = top.iter().find(|c| &c.kind == b"mvhd").ok_or_else(|| broken("the index has no movie header"))?;
    let s = mvhd.start;
    let (created, modified, timescale, duration, unknown, next_id_at) = if get(moov, s, 1)? == 1 {
        (get(moov, s + 4, 8)?, get(moov, s + 12, 8)?, get(moov, s + 20, 4)?, get(moov, s + 24, 8)?, u64::MAX, s + 108)
    } else {
        (
            get(moov, s + 4, 4)?,
            get(moov, s + 8, 4)?,
            get(moov, s + 12, 4)?,
            get(moov, s + 16, 4)?,
            u32::MAX.into(),
            s + 96,
        )
    };
    if next_id_at + 4 > mvhd.end {
        return Err(broken("the movie header is cut short"));
    }
    if timescale == 0 || duration == 0 || duration == unknown {
        return Err(broken("the movie has no length"));
    }
    let next_id = get(moov, next_id_at, 4)? as u32;
    let (mut max_id, mut max_group, mut tables, mut insert) = (0u32, 0i16, Vec::new(), None);
    for trak in top.iter().filter(|c| &c.kind == b"trak") {
        insert = Some(trak.end);
        let tkhd = child(moov, *trak, b"tkhd")?.ok_or_else(|| broken("a track has no header"))?;
        let wide = get(moov, tkhd.start, 1)? == 1;
        let (id_at, group_at) = if wide { (20, 46) } else { (12, 34) };
        if tkhd.start + group_at + 2 > tkhd.end {
            return Err(broken("a track header is cut short"));
        }
        max_id = max_id.max(get(moov, tkhd.start + id_at, 4)? as u32);
        max_group = max_group.max(get(moov, tkhd.start + group_at, 2)? as u16 as i16);
        let mut stbl = Some(*trak);
        for kind in [b"mdia", b"minf", b"stbl"] {
            stbl = match stbl {
                Some(parent) => child(moov, parent, kind)?,
                None => None,
            };
        }
        let stbl = stbl.ok_or_else(|| broken("a track has no sample table"))?;
        let mut found = 0;
        for table in children(moov, stbl.start, stbl.end)? {
            let width = match &table.kind {
                b"stco" => 4,
                b"co64" => 8,
                _ => continue,
            };
            let count = get(moov, table.start + 4, 4)? as usize;
            let first = table.start + 8;
            if first + count * width > table.end {
                return Err(broken("a chunk offset table is cut short"));
            }
            tables.push(Table { first, count, width });
            found += 1;
        }
        // Without its offsets a track's chunks could not be kept where they are.
        if found != 1 {
            return Err(broken("a track does not have one chunk offset table"));
        }
    }
    let insert = insert.ok_or_else(|| broken("the index holds no track"))?;
    let new_id = if next_id > max_id && next_id != u32::MAX {
        next_id
    } else {
        max_id.checked_add(1).ok_or_else(|| broken("no track id is left"))?
    };
    // The audio tracks share group 1; the subtitles take one of their own, as ffmpeg's do (3).
    let group = max_group.max(2).saturating_add(1);
    Ok(Index { header, insert, next_id_at, new_id, group, timescale, duration, created, modified, tables })
}

/// The new track's samples: the captions, and an empty sample for every gap, so the track covers
/// the whole movie and a player clears the last caption at its end.
struct Samples {
    durations: Vec<u32>,
    sizes: Vec<u32>,
    data: Vec<u8>,
}

impl Samples {
    fn new(cues: &[Cue], total_ms: u64) -> Samples {
        let mut samples = Samples { durations: Vec::new(), sizes: Vec::new(), data: Vec::new() };
        let mut time = 0;
        for cue in cues {
            samples.push(cue.start - time, "");
            samples.push(cue.end - cue.start, &cue.text);
            time = cue.end;
        }
        samples.push(total_ms - time, "");
        samples
    }

    fn push(&mut self, mut ms: u64, text: &str) {
        // A sample's text has a 16-bit length.
        let mut cut = text.len().min(u16::MAX as usize);
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut sample = (cut as u16).to_be_bytes().to_vec();
        sample.extend_from_slice(&text.as_bytes()[..cut]);
        while ms > 0 {
            let step = ms.min(u32::MAX.into());
            self.durations.push(step as u32);
            self.sizes.push(sample.len() as u32);
            self.data.extend_from_slice(&sample);
            ms -= step;
        }
    }
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// A box header for `payload` bytes, 64-bit when the size needs it.
fn box_header(kind: &[u8; 4], payload: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    match u32::try_from(payload + 8) {
        Ok(size) => out.extend_from_slice(&size.to_be_bytes()),
        Err(_) => {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(&(payload + 16).to_be_bytes());
            return out;
        }
    }
    out.extend_from_slice(kind);
    out
}

/// The parts of the new track that do not depend on where its chunk lands.
struct Plan {
    id: u32,
    group: i16,
    created: u64,
    modified: u64,
    /// In the movie's timescale.
    duration: u64,
    total_ms: u64,
    language: [u8; 3],
}

/// The text sample entry ffmpeg writes for `mov_text`: centred at the bottom, white 16-point
/// text on black, and a font table, which a 3GPP reader requires, naming one font.
fn tx3g() -> Vec<u8> {
    let mut b = vec![0u8; 6];
    b.extend_from_slice(&1u16.to_be_bytes()); // data reference index
    b.extend_from_slice(&0u32.to_be_bytes()); // display flags
    b.extend_from_slice(&[1, 0xff]); // horizontal centre, vertical bottom
    b.extend_from_slice(&[0, 0, 0, 0xff]); // background colour
    b.extend_from_slice(&[0; 8]); // default text box
    b.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 16]); // style: chars 0..0, font 1, plain, 16 point
    b.extend_from_slice(&[0xff; 4]); // text colour
    b.extend(mp4_box(b"ftab", &[0, 1, 0, 1, 5, b'A', b'r', b'i', b'a', b'l']));
    mp4_box(b"tx3g", &b)
}

fn text_trak(plan: &Plan, samples: &Samples, chunk: u64) -> Vec<u8> {
    let wide = |values: &[u64]| values.iter().any(|v| *v > u32::MAX.into());
    let times = |b: &mut Vec<u8>, version_1: bool, values: &[u64]| {
        for v in values {
            if version_1 {
                b.extend_from_slice(&v.to_be_bytes());
            } else {
                b.extend_from_slice(&(*v as u32).to_be_bytes());
            }
        }
    };

    let tkhd_1 = wide(&[plan.created, plan.modified, plan.duration]);
    // Flags 3: enabled and in the movie, as ffmpeg marks its first subtitle track.
    let mut tkhd = vec![u8::from(tkhd_1), 0, 0, 3];
    times(&mut tkhd, tkhd_1, &[plan.created, plan.modified]);
    tkhd.extend_from_slice(&plan.id.to_be_bytes());
    tkhd.extend_from_slice(&[0; 4]);
    times(&mut tkhd, tkhd_1, &[plan.duration]);
    tkhd.extend_from_slice(&[0; 8]);
    tkhd.extend_from_slice(&0i16.to_be_bytes()); // layer
    tkhd.extend_from_slice(&plan.group.to_be_bytes());
    tkhd.extend_from_slice(&[0; 4]); // volume, reserved
    for m in [0x1_0000u32, 0, 0, 0, 0x1_0000, 0, 0, 0, 0x4000_0000] {
        tkhd.extend_from_slice(&m.to_be_bytes());
    }
    tkhd.extend_from_slice(&[0; 8]); // width and height: the player places the text

    let mdhd_1 = wide(&[plan.created, plan.modified, plan.total_ms]);
    let mut mdhd = vec![u8::from(mdhd_1), 0, 0, 0];
    times(&mut mdhd, mdhd_1, &[plan.created, plan.modified]);
    mdhd.extend_from_slice(&(TEXT_TIMESCALE as u32).to_be_bytes());
    times(&mut mdhd, mdhd_1, &[plan.total_ms]);
    let packed = plan.language.iter().fold(0u16, |v, c| v << 5 | (u16::from(*c).wrapping_sub(0x60) & 31));
    mdhd.extend_from_slice(&packed.to_be_bytes());
    mdhd.extend_from_slice(&[0; 2]);

    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"sbtl");
    hdlr.extend_from_slice(&[0; 12]);
    hdlr.extend_from_slice(b"SubtitleHandler\0");

    let url = mp4_box(b"url ", &[0, 0, 0, 1]); // the samples are in this file
    let dref = mp4_box(b"dref", &[&[0, 0, 0, 0, 0, 0, 0, 1], url.as_slice()].concat());

    let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
    stsd.extend(tx3g());
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for d in &samples.durations {
        match runs.last_mut() {
            Some((count, delta)) if delta == d => *count += 1,
            _ => runs.push((1, *d)),
        }
    }
    let mut stts = vec![0u8; 4];
    stts.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, delta) in runs {
        stts.extend_from_slice(&count.to_be_bytes());
        stts.extend_from_slice(&delta.to_be_bytes());
    }
    let count = samples.sizes.len() as u32;
    let mut stsc = vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1];
    stsc.extend_from_slice(&count.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes());
    let mut stsz = vec![0u8; 8];
    stsz.extend_from_slice(&count.to_be_bytes());
    for size in &samples.sizes {
        stsz.extend_from_slice(&size.to_be_bytes());
    }
    let offsets = match u32::try_from(chunk) {
        Ok(small) => mp4_box(b"stco", &[&[0, 0, 0, 0, 0, 0, 0, 1], small.to_be_bytes().as_slice()].concat()),
        Err(_) => mp4_box(b"co64", &[&[0, 0, 0, 0, 0, 0, 0, 1], chunk.to_be_bytes().as_slice()].concat()),
    };
    let stbl =
        [mp4_box(b"stsd", &stsd), mp4_box(b"stts", &stts), mp4_box(b"stsc", &stsc), mp4_box(b"stsz", &stsz), offsets]
            .concat();

    let minf = [mp4_box(b"nmhd", &[0; 4]), mp4_box(b"dinf", &dref), mp4_box(b"stbl", &stbl)].concat();
    let mdia = [mp4_box(b"mdhd", &mdhd), mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &minf)].concat();
    mp4_box(b"trak", &[mp4_box(b"tkhd", &tkhd), mp4_box(b"mdia", &mdia)].concat())
}

/// The new track for a chunk that lands where `place` says for a track of a given length. A
/// chunk past 4 GiB needs `co64`, which lengthens the track, so the place is asked again.
fn settle(plan: &Plan, samples: &Samples, place: impl Fn(usize) -> u64) -> Vec<u8> {
    let mut trak = text_trak(plan, samples, 0);
    loop {
        let next = text_trak(plan, samples, place(trak.len()));
        if next.len() == trak.len() {
            return next;
        }
        trak = next;
    }
}

/// The index with the new track and the next free track id, and every chunk offset from
/// `moved_from` on moved by `delta`; `None` when a moved offset no longer fits its table.
fn moov_with(old: &[u8], index: &Index, trak: &[u8], moved_from: u64, delta: i128) -> Option<Vec<u8>> {
    let base = index.header;
    let mut payload = old[base..].to_vec();
    put(&mut payload, index.next_id_at - base, 4, (u64::from(index.new_id) + 1).min(u32::MAX.into()));
    for table in &index.tables {
        let limit = if table.width == 4 { u32::MAX.into() } else { u64::MAX };
        for i in 0..table.count {
            let at = table.first - base + i * table.width;
            let value = payload[at..at + table.width].iter().fold(0u64, |v, b| v << 8 | u64::from(*b));
            if value >= moved_from {
                let moved = u64::try_from(i128::from(value) + delta).ok().filter(|m| *m <= limit)?;
                put(&mut payload, at, table.width, moved);
            }
        }
    }
    let at = index.insert - base;
    payload.splice(at..at, trak.iter().copied());
    let mut moov = Vec::with_capacity(8 + payload.len());
    moov.extend_from_slice(&u32::try_from(8 + payload.len()).ok()?.to_be_bytes());
    moov.extend_from_slice(b"moov");
    moov.extend(payload);
    Some(moov)
}

/// Adds a tx3g subtitle track holding `captions` to the finished, non-fragmented MP4 at `path`.
/// The captions are cut to the movie's length; with none left, the file stays as it is. A file
/// this cannot change correctly (fragmented, malformed) is refused and left as it is.
pub fn add_track(path: &Path, captions: &[Caption], language: &str) -> Result<()> {
    if captions.is_empty() {
        return Ok(());
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let length = file.metadata()?.len();
    let boxes = top_level(&mut file, length)?;
    if boxes.iter().any(|b| matches!(&b.kind, b"moof" | b"mfra")) {
        return Err(broken("it is fragmented"));
    }
    let mut moovs = boxes.iter().filter(|b| &b.kind == b"moov");
    let (Some(moov), None) = (moovs.next(), moovs.next()) else {
        return Err(broken("it does not hold exactly one index"));
    };
    if moov.size > MAX_MOOV_BYTES {
        return Err(broken("its index is too large"));
    }
    let mut old = vec![0u8; moov.size as usize];
    file.seek(SeekFrom::Start(moov.offset))?;
    file.read_exact(&mut old)?;
    let index = parse(&old)?;

    let total_ms = (u128::from(index.duration) * u128::from(TEXT_TIMESCALE) / u128::from(index.timescale)) as u64;
    let cues = cues(captions, total_ms);
    if cues.is_empty() {
        return Ok(());
    }
    let samples = Samples::new(&cues, total_ms);
    let plan = Plan {
        id: index.new_id,
        group: index.group,
        created: index.created,
        modified: index.modified,
        // Rounded down, so the track never outlasts the movie.
        duration: (u128::from(total_ms) * u128::from(index.timescale) / u128::from(TEXT_TIMESCALE)) as u64,
        total_ms,
        language: iso639_2(language),
    };

    let moov_end = moov.offset + moov.size;
    let mut behind = false;
    for table in &index.tables {
        for i in 0..table.count {
            let offset = get(&old, table.first + i * table.width, table.width)?;
            if (moov.offset..moov_end).contains(&offset) || offset >= length {
                return Err(broken("a chunk lies outside the media"));
            }
            behind |= offset >= moov_end;
        }
    }
    let edit = Edit { boxes: &boxes, moov, old: &old, index: &index, plan: &plan, samples: &samples };
    if behind {
        index_first(path, file, &edit)
    } else {
        index_last(file, &edit)
    }
}

/// The file as it was read, and the track that goes into it.
struct Edit<'a> {
    boxes: &'a [TopBox],
    moov: &'a TopBox,
    old: &'a [u8],
    index: &'a Index,
    plan: &'a Plan,
    samples: &'a Samples,
}

/// Writes the new index and the text where the old index lay, in place: the media in front of it
/// does not move.
fn index_last(mut file: File, edit: &Edit) -> Result<()> {
    let Edit { boxes, moov, old, index, plan, samples } = *edit;
    let length = file.metadata()?.len();
    let moov_end = moov.offset + moov.size;
    let text_header = box_header(b"mdat", samples.data.len() as u64);
    let trak = settle(plan, samples, |_| moov.offset + text_header.len() as u64);
    let new_moov = moov_with(old, index, &trak, moov_end, 0).ok_or_else(|| broken("its index is too large"))?;
    // Padding behind the index goes; anything else there (metadata another tool appended) stays.
    let mut kept = Vec::new();
    for b in boxes.iter().filter(|b| b.offset >= moov_end && !matches!(&b.kind, b"free" | b"skip" | b"wide")) {
        if kept.len() as u64 + b.size > MAX_MOOV_BYTES {
            return Err(broken("too much follows its index"));
        }
        let mut bytes = vec![0u8; b.size as usize];
        file.seek(SeekFrom::Start(b.offset))?;
        file.read_exact(&mut bytes)?;
        if b.open {
            // It no longer ends the file once padding behind it is gone, so it gets its size.
            put(&mut bytes, 0, 4, b.size);
        }
        kept.extend(bytes);
    }
    let tail = [text_header, samples.data.clone(), new_moov, kept].concat();
    let end = moov.offset + tail.len() as u64;

    // The bytes the new tail covers, to put back when writing fails half way.
    let mut saved = vec![0u8; (length.min(end) - moov.offset) as usize];
    file.seek(SeekFrom::Start(moov.offset))?;
    file.read_exact(&mut saved)?;
    let mut place = |at: u64, bytes: &[u8], length: u64| -> io::Result<()> {
        file.seek(SeekFrom::Start(at))?;
        file.write_all(bytes)?;
        file.set_len(length)?;
        file.sync_all()
    };
    if let Err(e) = place(moov.offset, &tail, end) {
        let _ = place(moov.offset, &saved, length);
        return Err(e.into());
    }
    Ok(())
}

/// Copies the file with the new index where the old one was and the text right behind it, so
/// the index still comes before the media. When a chunk would then move past what `stco` can
/// hold, the index goes to the end instead, and the media moves towards the start.
fn index_first(path: &Path, file: File, edit: &Edit) -> Result<()> {
    let Edit { boxes, moov, old, index, plan, samples } = *edit;
    let length = file.metadata()?.len();
    let moov_end = moov.offset + moov.size;
    let text_header = box_header(b"mdat", samples.data.len() as u64);
    let text_len = (text_header.len() + samples.data.len()) as u64;
    let moov_len = |trak_len: usize| (8 + old.len() - index.header + trak_len) as u64;

    let trak = settle(plan, samples, |trak_len| moov.offset + moov_len(trak_len) + text_header.len() as u64);
    let delta = i128::from(moov_len(trak.len()) + text_len) - i128::from(moov.size);
    if let Some(new_moov) = moov_with(old, index, &trak, moov_end, delta) {
        let parts = [
            Part::Copy(0, moov.offset),
            Part::Bytes(&new_moov),
            Part::Bytes(&text_header),
            Part::Bytes(&samples.data),
            Part::Copy(moov_end, length),
        ];
        return rewrite(path, file, &parts);
    }

    let text_at = length - moov.size;
    let trak = settle(plan, samples, |_| text_at + text_header.len() as u64);
    let new_moov = moov_with(old, index, &trak, moov_end, -i128::from(moov.size))
        .ok_or_else(|| broken("its index is too large"))?;
    let mut parts = vec![Part::Copy(0, moov.offset)];
    let sized: [u8; 8];
    match boxes.last() {
        // The last box runs to the end of the file, which now goes on behind it.
        Some(last) if last.open => {
            let size = u32::try_from(last.size).map_err(|_| broken("its last box has no size"))?;
            sized = [size.to_be_bytes(), last.kind].concat().try_into().unwrap_or_default();
            parts.extend([Part::Copy(moov_end, last.offset), Part::Bytes(&sized), Part::Copy(last.offset + 8, length)]);
        }
        _ => parts.push(Part::Copy(moov_end, length)),
    }
    parts.extend([Part::Bytes(&text_header), Part::Bytes(&samples.data), Part::Bytes(&new_moov)]);
    rewrite(path, file, &parts)
}

/// A piece of the new file: a range of the old one, or new bytes.
enum Part<'a> {
    Copy(u64, u64),
    Bytes(&'a [u8]),
}

impl Part<'_> {
    fn len(&self) -> u64 {
        match self {
            Part::Copy(from, to) => to - from,
            Part::Bytes(bytes) => bytes.len() as u64,
        }
    }
}

/// Writes `parts` into a sibling of `path` and gives it the name `path`, so the file under that
/// name is always whole; a failure leaves `path` as it was and removes the sibling.
fn rewrite(path: &Path, source: File, parts: &[Part]) -> Result<()> {
    let total: u64 = parts.iter().map(Part::len).sum();
    if let Some(free) = path.parent().and_then(|dir| files::free_bytes(dir).ok()) {
        if free < total.saturating_add(files::LOW_DISK_BYTES) {
            return Err(RecordError::DiskFull { path: path.to_path_buf(), free_bytes: free });
        }
    }
    let temp = files::part_path(path, "captions");
    // A sibling under this name is a leftover of this same export that was cut short.
    let _ = std::fs::remove_file(&temp);
    let written = write_parts(&source, &temp, parts, total);
    // Windows replaces a file by name only once no handle holds it.
    drop(source);
    let renamed = written.and_then(|()| std::fs::rename(&temp, path).map_err(RecordError::from));
    if renamed.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    renamed?;
    if let Some(dir) = path.parent() {
        // Makes the rename itself durable where the system can sync a folder.
        let _ = File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

fn write_parts(source: &File, temp: &Path, parts: &[Part], total: u64) -> Result<()> {
    let target = OpenOptions::new().write(true).create_new(true).open(temp)?;
    let mut out = BufWriter::with_capacity(COPY_BUFFER, target);
    let mut reader = source;
    for part in parts {
        match *part {
            Part::Bytes(bytes) => out.write_all(bytes)?,
            Part::Copy(from, to) => {
                reader.seek(SeekFrom::Start(from))?;
                let copied = io::copy(&mut reader.take(to - from), &mut out)?;
                if copied != to - from {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the media ended early while it was copied",
                    )
                    .into());
                }
            }
        }
    }
    let target = out.into_inner().map_err(io::IntoInnerError::into_error)?;
    target.sync_all()?;
    if target.metadata()?.len() != total {
        return Err(broken("the copy came out at the wrong length"));
    }
    Ok(())
}
