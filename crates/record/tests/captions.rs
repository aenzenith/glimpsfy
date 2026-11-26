//! The subtitle track an export gains leaves every chunk of the tracks it already had where their
//! offsets say, whether the index came before the media or after it. A wrong offset would play
//! another track's bytes, or garbage, with nothing to say the file was damaged.

use std::path::Path;

use glimpsfy_record::captions::{add_track, Caption};

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    [&((8 + body.len()) as u32).to_be_bytes()[..], kind, body].concat()
}

fn full(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    mp4_box(kind, &[&[0, 0, 0, 0], body].concat())
}

fn be(data: &[u8], at: usize, width: usize) -> u64 {
    data[at..at + width].iter().fold(0, |v, b| v << 8 | u64::from(*b))
}

/// A track of four-byte samples, one per chunk, at `offsets`, in `co64` when `wide`.
fn trak(id: u32, handler: &[u8; 4], group: u16, offsets: &[u64], wide: bool) -> Vec<u8> {
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&id.to_be_bytes());
    tkhd.extend_from_slice(&[0; 4]);
    tkhd.extend_from_slice(&3000u32.to_be_bytes());
    tkhd.extend_from_slice(&[0; 10]);
    tkhd.extend_from_slice(&group.to_be_bytes());
    tkhd.extend_from_slice(&[0; 4]);
    for m in [0x1_0000u32, 0, 0, 0, 0x1_0000, 0, 0, 0, 0x4000_0000] {
        tkhd.extend_from_slice(&m.to_be_bytes());
    }
    tkhd.extend_from_slice(&[0; 8]);
    let n = offsets.len() as u32;
    let mdhd = [&[0u8; 8][..], &1000u32.to_be_bytes(), &3000u32.to_be_bytes(), &[0x55, 0xc4, 0, 0]].concat();
    let hdlr = [&[0u8; 4][..], handler, &[0; 13]].concat();
    let stts = [1u32.to_be_bytes(), n.to_be_bytes(), (3000 / n).to_be_bytes()].concat();
    let stsc = [1u32.to_be_bytes(), 1u32.to_be_bytes(), 1u32.to_be_bytes(), 1u32.to_be_bytes()].concat();
    let stsz = [4u32.to_be_bytes(), n.to_be_bytes()].concat();
    let mut chunks = n.to_be_bytes().to_vec();
    for offset in offsets {
        if wide {
            chunks.extend_from_slice(&offset.to_be_bytes());
        } else {
            chunks.extend_from_slice(&(*offset as u32).to_be_bytes());
        }
    }
    let stbl = [
        full(b"stsd", &[0, 0, 0, 0]),
        full(b"stts", &stts),
        full(b"stsc", &stsc),
        full(b"stsz", &stsz),
        full(if wide { b"co64" } else { b"stco" }, &chunks),
    ]
    .concat();
    let minf = mp4_box(b"minf", &mp4_box(b"stbl", &stbl));
    let mdia = mp4_box(b"mdia", &[full(b"mdhd", &mdhd), full(b"hdlr", &hdlr), minf].concat());
    mp4_box(b"trak", &[full(b"tkhd", &tkhd), mdia].concat())
}

/// The chunks of the video track (`V0`…) and of the audio track (`A0`…), interleaved.
const SAMPLES: [&[u8; 4]; 5] = [b"V0V0", b"A0A0", b"V1V1", b"A1A1", b"V2V2"];

/// An index for 3 s of movie whose chunks start at `data`, the first payload byte of `mdat`.
fn moov(data: u64) -> Vec<u8> {
    let at = |i: u64| data + i * 4;
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&3000u32.to_be_bytes());
    mvhd.extend_from_slice(&[0; 76]);
    mvhd.extend_from_slice(&3u32.to_be_bytes());
    let video = trak(1, b"vide", 0, &[at(0), at(2), at(4)], false);
    let audio = trak(2, b"soun", 1, &[at(1), at(3)], true);
    mp4_box(b"moov", &[full(b"mvhd", &mvhd), video, audio].concat())
}

/// The file in both layouts: the index first with padding and a 64-bit `mdat` header behind it,
/// and the index last behind a plain `mdat`.
fn file(index_first: bool) -> Vec<u8> {
    let ftyp = mp4_box(b"ftyp", b"isom\0\0\x02\0isommp42");
    let payload: Vec<u8> = SAMPLES.iter().flat_map(|s| s.iter().copied()).collect();
    let size = moov(0).len() as u64;
    if index_first {
        let free = mp4_box(b"free", &[0; 8]);
        let data = ftyp.len() as u64 + size + free.len() as u64 + 16;
        let mdat = [&1u32.to_be_bytes()[..], b"mdat", &(16 + payload.len() as u64).to_be_bytes(), &payload].concat();
        [ftyp, moov(data), free, mdat].concat()
    } else {
        let data = ftyp.len() as u64 + 8;
        [ftyp.clone(), mp4_box(b"mdat", &payload), moov(data)].concat()
    }
}

/// The boxes in `data[from..to]` as (type, payload start, payload end).
fn boxes(data: &[u8], from: usize, to: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut at = from;
    while at + 8 <= to {
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        let (header, size) = match be(data, at, 4) {
            1 => (16, be(data, at + 8, 8) as usize),
            0 => (8, to - at),
            n => (8, n as usize),
        };
        assert!(size >= header && at + size <= to, "a box runs past its parent");
        out.push((kind, at + header, at + size));
        at += size;
    }
    out
}

fn find(data: &[u8], parent: (usize, usize), path: &[&[u8; 4]]) -> Vec<(usize, usize)> {
    let Some((first, rest)) = path.split_first() else { return vec![parent] };
    boxes(data, parent.0, parent.1)
        .into_iter()
        .filter(|(kind, ..)| kind == *first)
        .flat_map(|(_, start, end)| find(data, (start, end), rest))
        .collect()
}

fn one(data: &[u8], parent: (usize, usize), path: &[&[u8; 4]]) -> (usize, usize) {
    let found = find(data, parent, path);
    assert_eq!(found.len(), 1, "{path:?}");
    found[0]
}

/// The track's chunk offsets, from `stco` or `co64`.
fn chunks(data: &[u8], stbl: (usize, usize)) -> Vec<u64> {
    let (table, width) = match find(data, stbl, &[b"stco"]).first() {
        Some(&table) => (table, 4),
        None => (one(data, stbl, &[b"co64"]), 8),
    };
    (0..be(data, table.0 + 4, 4) as usize).map(|i| be(data, table.0 + 8 + i * width, width)).collect()
}

#[test]
fn chunks_stay_on_their_samples_and_the_captions_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let captions = [
        // The gap in front of the first caption becomes an empty sample.
        Caption { start_ms: 500, end_ms: 1200, text: "Merhaba ğüşıöç İ".into() },
        Caption { start_ms: 1500, end_ms: 2500, text: "iki\nsatır 🎬".into() },
        // Cut to the movie's 3 s.
        Caption { start_ms: 2800, end_ms: 9000, text: "past the end".into() },
    ];
    let expected = [(500, 1200, "Merhaba ğüşıöç İ"), (1500, 2500, "iki\nsatır 🎬"), (2800, 3000, "past the end")];

    for index_first in [true, false] {
        let path = dir.path().join(format!("export-{index_first}.mp4"));
        std::fs::write(&path, file(index_first)).unwrap();
        add_track(&path, &captions, "tr").unwrap();
        let data = std::fs::read(&path).unwrap();
        let top = boxes(&data, 0, data.len());
        assert_eq!(top.last().map(|(_, _, end)| *end), Some(data.len()), "the boxes fill the file");

        let kinds: Vec<[u8; 4]> = top.iter().map(|(kind, ..)| *kind).collect();
        let moov_at = kinds.iter().position(|k| k == b"moov").unwrap();
        let last_mdat = kinds.iter().rposition(|k| k == b"mdat").unwrap();
        assert_eq!(moov_at < last_mdat, index_first, "the index keeps its place: {kinds:?}");

        let moov = (top[moov_at].1, top[moov_at].2);
        let mvhd = one(&data, moov, &[b"mvhd"]);
        assert_eq!(be(&data, mvhd.0 + 96, 4), 4, "the next track id moved on");

        let traks = find(&data, moov, &[b"trak"]);
        assert_eq!(traks.len(), 3);
        for (trak, samples) in traks[..2].iter().zip([[0, 2, 4].as_slice(), [1, 3].as_slice()]) {
            let offsets = chunks(&data, one(&data, *trak, &[b"mdia", b"minf", b"stbl"]));
            let read: Vec<&[u8]> = offsets.iter().map(|&o| &data[o as usize..o as usize + 4]).collect();
            let wanted: Vec<&[u8]> = samples.iter().map(|&i| SAMPLES[i].as_slice()).collect();
            assert_eq!(read, wanted, "index first: {index_first}");
        }

        let text = traks[2];
        let tkhd = one(&data, text, &[b"tkhd"]);
        assert_eq!(be(&data, tkhd.0 + 12, 4), 3, "the new track takes the next id");
        let mdia = one(&data, text, &[b"mdia"]);
        let hdlr = one(&data, mdia, &[b"hdlr"]);
        assert_eq!(&data[hdlr.0 + 8..hdlr.0 + 12], b"sbtl");
        let stbl = one(&data, mdia, &[b"minf", b"stbl"]);
        let stts = one(&data, stbl, &[b"stts"]);
        let mut durations = Vec::new();
        for i in 0..be(&data, stts.0 + 4, 4) as usize {
            let at = stts.0 + 8 + i * 8;
            durations.extend(std::iter::repeat_n(be(&data, at + 4, 4), be(&data, at, 4) as usize));
        }
        let stsz = one(&data, stbl, &[b"stsz"]);
        assert_eq!(be(&data, stsz.0 + 8, 4) as usize, durations.len());
        assert_eq!(durations.iter().sum::<u64>(), 3000, "the samples cover the movie");
        let [chunk] = chunks(&data, stbl)[..] else { panic!("the text is not in one chunk") };
        let (mut at, mut time, mut read) = (chunk as usize, 0, Vec::new());
        for (i, duration) in durations.iter().enumerate() {
            let size = be(&data, stsz.0 + 12 + i * 4, 4) as usize;
            let length = be(&data, at, 2) as usize;
            assert_eq!(length + 2, size);
            if length > 0 {
                read.push((time, time + duration, std::str::from_utf8(&data[at + 2..at + size]).unwrap()));
            }
            at += size;
            time += duration;
        }
        assert_eq!(read, expected, "index first: {index_first}");
        assert!(!has_leftovers(dir.path()), "the working copy was removed");
    }
}

fn has_leftovers(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".part"))
}
