//! The history deletes user data, so these tests cover what would fail silently and could not be
//! undone: what a sweep and a delete remove, migrations of older databases, the Turkish search,
//! the crash invariant, and an edit revised by later outputs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use glimpsfy_upload::{DeletionHandle, ProviderId};
use rusqlite::Connection;

use crate::*;

const DAY: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 1_794_000_000_000;

/// A Trash that moves files into a folder of its own and remembers what it was given.
struct FakeTrash {
    folder: PathBuf,
    moved: Mutex<Vec<PathBuf>>,
    fail: bool,
}

impl Trash for FakeTrash {
    fn trash(&self, path: &Path) -> io::Result<()> {
        if self.fail {
            return Err(io::Error::other("the Trash refused"));
        }
        let target = self.folder.join(format!(
            "{}-{}",
            self.moved.lock().unwrap().len(),
            path.file_name().unwrap().to_string_lossy()
        ));
        fs::rename(path, target)?;
        self.moved.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

/// How the fake host answers a deletion.
#[derive(Clone, Copy, PartialEq)]
enum Host {
    Deletes,
    Fails,
    /// Deletes on its own web page, as ImgBB does.
    OpensPage,
}

struct Hosts {
    host: Host,
    calls: Mutex<Vec<EntryId>>,
}

impl HostedCopies for Hosts {
    fn delete(&self, entry: &Entry) -> std::result::Result<HostDeletion, String> {
        self.calls.lock().unwrap().push(entry.id.clone());
        match self.host {
            Host::Deletes => Ok(HostDeletion::Deleted),
            Host::Fails => Err("the host answered 503".into()),
            Host::OpensPage => Ok(HostDeletion::PageOpened { url: "https://ibb.co/delete/oK3q".into() }),
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: HistoryStore,
    trash: Arc<FakeTrash>,
    /// A folder outside the history, standing for ~/Pictures/Glimpsfy.
    user: PathBuf,
}

fn fixture(trash_fails: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let trash =
        Arc::new(FakeTrash { folder: dir.path().join("Trash"), moved: Mutex::new(Vec::new()), fail: trash_fails });
    fs::create_dir_all(&trash.folder).unwrap();
    let user = dir.path().join("Pictures");
    fs::create_dir_all(&user).unwrap();
    let store = HistoryStore::open(HistoryPaths::new(dir.path().join("history")), trash.clone()).unwrap();
    Fixture { _dir: dir, store, trash, user }
}

fn screenshot(name: &str, created_at_ms: i64) -> NewEntry {
    NewEntry {
        kind: Some(EntryKind::Screenshot),
        name: name.into(),
        created_at_ms,
        size: Some(PixelSize { width: 40, height: 20 }),
        ..NewEntry::default()
    }
}

fn thumbnail() -> thumbnail::Thumbnail {
    thumbnail::Thumbnail { bytes: b"jpeg".to_vec(), format: thumbnail::ThumbnailFormat::Jpeg, width: 4, height: 2 }
}

fn hosted() -> HostedCopy {
    HostedCopy {
        provider: ProviderId::new("0x0"),
        url: "https://0x0.st/oK3q.png".into(),
        deletion: Some(DeletionHandle::Token { token: "secret-token".into(), url: None }),
        uploaded_at_ms: NOW - DAY,
        expires_at_ms: None,
    }
}

/// An entry that owns a copy in the captures folder and a thumbnail, and optionally names a
/// file the user saved.
fn add_owned(f: &Fixture, name: &str, created_at_ms: i64, saved: bool) -> Entry {
    let mut new = screenshot(name, created_at_ms);
    if saved {
        let path = f.user.join(format!("{name}.png"));
        fs::write(&path, b"user's png").unwrap();
        new.saved_path = Some(path);
    }
    let entry = f.store.add(new, OwnFile::Bytes { bytes: b"own png", extension: "png" }).unwrap();
    f.store.set_thumbnail(&entry.id, &thumbnail()).unwrap()
}

fn ids(page: &HistoryPage) -> Vec<String> {
    page.items.iter().map(|i| i.entry.name.clone()).collect()
}

/// The sweep removes exactly the unpinned entries older than the period, with their own files,
/// and nothing else: never a pinned entry, never a file outside the history's folder unless the
/// option says so (and then only to the Trash), and nothing at all with "forever" or with the
/// history turned off.
#[test]
fn the_sweep_removes_only_unpinned_entries_older_than_the_period() {
    struct Case {
        keep: Retention,
        trash_saved: bool,
        /// Names of the entries the sweep must remove.
        removed: &'static [&'static str],
    }
    let cases = [
        Case { keep: Retention::Off, trash_saved: true, removed: &[] },
        Case { keep: Retention::Forever, trash_saved: true, removed: &[] },
        Case {
            keep: Retention::Days90,
            trash_saved: false,
            removed: &["100 days", "100 days saved", "100 days damaged"],
        },
        Case {
            keep: Retention::Days30,
            trash_saved: false,
            removed: &["31 days", "100 days", "100 days saved", "100 days damaged"],
        },
        Case {
            keep: Retention::Days7,
            trash_saved: true,
            removed: &["8 days", "31 days", "100 days", "100 days saved", "100 days damaged"],
        },
    ];
    for case in cases {
        let f = fixture(false);
        let mut all = Vec::new();
        for (name, age) in [
            ("today", 0),
            ("7 days exactly", 7 * DAY),
            ("8 days", 8 * DAY),
            ("31 days", 31 * DAY),
            ("100 days", 100 * DAY),
        ] {
            all.push(add_owned(&f, name, NOW - age, false));
        }
        let saved = add_owned(&f, "100 days saved", NOW - 100 * DAY, true);
        all.push(saved.clone());
        let pinned = add_owned(&f, "100 days pinned", NOW - 100 * DAY, true);
        f.store.set_pinned(&pinned.id, true).unwrap();
        all.push(pinned.clone());
        // A damaged row whose "own" file points at a file of the user's.
        let damaged = add_owned(&f, "100 days damaged", NOW - 100 * DAY, false);
        let victim = f.user.join("precious.png");
        fs::write(&victim, b"precious").unwrap();
        Connection::open(f.store.paths().database())
            .unwrap()
            .execute("UPDATE entries SET file = ?1 WHERE id = ?2", [victim.to_string_lossy().as_ref(), &damaged.id.0])
            .unwrap();
        all.push(damaged.clone());

        let report = f.store.sweep(NOW, case.keep, SweepOptions { trash_saved_files: case.trash_saved }).unwrap();

        let label = format!("{:?}", case.keep);
        let mut removed: Vec<String> =
            report.removed().iter().map(|id| all.iter().find(|e| &e.id == id).unwrap().name.clone()).collect();
        removed.sort();
        let mut expected: Vec<String> = case.removed.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(removed, expected, "{label}");
        for entry in &all {
            let gone = case.removed.contains(&entry.name.as_str());
            assert_eq!(f.store.get(&entry.id).is_err(), gone, "{label}: {}", entry.name);
            if entry.id != damaged.id {
                assert_eq!(entry.file.as_ref().unwrap().exists(), !gone, "{label}: own copy of {}", entry.name);
            }
            assert_eq!(entry.thumbnail.as_ref().unwrap().exists(), !gone, "{label}: thumbnail of {}", entry.name);
        }
        // The pinned entry's saved file is never touched, and the user's file behind a damaged
        // row is never unlinked.
        assert!(pinned.saved_path.as_ref().unwrap().exists(), "{label}");
        assert_eq!(fs::read(&victim).unwrap(), b"precious", "{label}");
        // The saved file of a removed entry goes to the Trash only with the option.
        let saved_gone = case.removed.contains(&"100 days saved") && case.trash_saved;
        assert_eq!(saved.saved_path.as_ref().unwrap().exists(), !saved_gone, "{label}");
        assert_eq!(
            *f.trash.moved.lock().unwrap(),
            if saved_gone { vec![saved.saved_path.clone().unwrap()] } else { vec![] },
            "{label}"
        );
    }
}

/// A delete removes the row and the history's own files; the saved file goes to the Trash only
/// when asked and is never unlinked; a hosted copy whose deletion fails, or waits for the user on
/// the host's own page, keeps the entry with its link, and the report says per entry what was and
/// was not removed.
#[test]
fn a_delete_removes_what_it_was_asked_to_and_keeps_the_link_when_the_host_fails() {
    struct Case {
        trash_saved_file: bool,
        delete_hosted_copy: bool,
        host: Host,
        trash_fails: bool,
        removed: bool,
        saved: StepOutcome,
        hosted: StepOutcome,
    }
    let failed = |reason: &str| StepOutcome::Failed { reason: reason.into() };
    let cases = [
        Case {
            trash_saved_file: false,
            delete_hosted_copy: false,
            host: Host::Deletes,
            trash_fails: false,
            removed: true,
            saved: StepOutcome::Untouched,
            hosted: StepOutcome::Untouched,
        },
        Case {
            trash_saved_file: true,
            delete_hosted_copy: false,
            host: Host::Deletes,
            trash_fails: false,
            removed: true,
            saved: StepOutcome::Removed,
            hosted: StepOutcome::Untouched,
        },
        Case {
            trash_saved_file: true,
            delete_hosted_copy: true,
            host: Host::Deletes,
            trash_fails: false,
            removed: true,
            saved: StepOutcome::Removed,
            hosted: StepOutcome::Removed,
        },
        Case {
            trash_saved_file: true,
            delete_hosted_copy: true,
            host: Host::Fails,
            trash_fails: false,
            removed: false,
            saved: StepOutcome::Skipped,
            hosted: failed("the host answered 503"),
        },
        Case {
            trash_saved_file: false,
            delete_hosted_copy: true,
            host: Host::Fails,
            trash_fails: false,
            removed: false,
            saved: StepOutcome::Untouched,
            hosted: failed("the host answered 503"),
        },
        Case {
            trash_saved_file: true,
            delete_hosted_copy: false,
            host: Host::Deletes,
            trash_fails: true,
            removed: false,
            saved: failed("the Trash refused"),
            hosted: StepOutcome::Untouched,
        },
        Case {
            trash_saved_file: true,
            delete_hosted_copy: true,
            host: Host::OpensPage,
            trash_fails: false,
            removed: false,
            saved: StepOutcome::Skipped,
            hosted: StepOutcome::PageOpened { url: "https://ibb.co/delete/oK3q".into() },
        },
    ];
    for (n, case) in cases.into_iter().enumerate() {
        let f = fixture(case.trash_fails);
        let entry = add_owned(&f, "Northwind", NOW, true);
        let entry = f.store.set_hosted(&entry.id, Some(hosted())).unwrap();
        let bystander = add_owned(&f, "Bystander", NOW, true);
        let hosts = Hosts { host: case.host, calls: Mutex::new(Vec::new()) };
        let options =
            DeleteOptions { trash_saved_file: case.trash_saved_file, delete_hosted_copy: case.delete_hosted_copy };

        // The id twice and an unknown id: both are harmless.
        let report =
            f.store.delete(&[entry.id.clone(), entry.id.clone(), EntryId("ffff".into())], &options, &hosts).unwrap();

        assert_eq!(report.outcomes.len(), 1, "case {n}");
        let outcome = &report.outcomes[0];
        assert_eq!(outcome.removed, case.removed, "case {n}");
        assert_eq!(outcome.saved_file, case.saved, "case {n}");
        assert_eq!(outcome.hosted_copy, case.hosted, "case {n}");
        let (own, thumb, saved) =
            (entry.file.as_ref().unwrap(), entry.thumbnail.as_ref().unwrap(), entry.saved_path.as_ref().unwrap());
        if case.removed {
            assert_eq!(outcome.own_files, StepOutcome::Removed, "case {n}");
            assert!(matches!(f.store.get(&entry.id), Err(HistoryError::NotFound(_))), "case {n}");
            assert!(!own.exists() && !thumb.exists(), "case {n}");
            assert_eq!(report.freed_bytes, b"own png".len() as u64 + b"jpeg".len() as u64, "case {n}");
        } else {
            let still = f.store.get(&entry.id).unwrap();
            assert!(own.exists() && thumb.exists(), "case {n}");
            if case.host != Host::Deletes {
                // The link and what deleting it needs are still there for a retry, or for the user
                // to confirm the deletion they finished on the host's page.
                assert_eq!(still.hosted, Some(hosted()), "case {n}");
            }
        }
        // The user's file is in the Trash only when that was asked and worked, and never deleted.
        let trashed = case.saved == StepOutcome::Removed;
        assert_eq!(saved.exists(), !trashed, "case {n}");
        assert_eq!(f.trash.moved.lock().unwrap().len(), trashed as usize, "case {n}");
        assert_eq!(
            *hosts.calls.lock().unwrap(),
            if case.delete_hosted_copy { vec![entry.id.clone()] } else { vec![] },
            "case {n}"
        );
        // Nothing else was touched.
        assert!(f.store.get(&bystander.id).is_ok() && bystander.saved_path.as_ref().unwrap().exists(), "case {n}");
    }

    // Clearing keeps pinned entries, the files the user saved and the hosted copies.
    let f = fixture(false);
    let a = add_owned(&f, "a", NOW, true);
    let b = add_owned(&f, "b", NOW, true);
    f.store.set_pinned(&b.id, true).unwrap();
    let report = f.store.clear().unwrap();
    assert_eq!(report.removed(), vec![a.id.clone()]);
    assert!(a.saved_path.as_ref().unwrap().exists() && f.trash.moved.lock().unwrap().is_empty());
    assert!(f.store.get(&b.id).is_ok() && b.file.as_ref().unwrap().exists());
}

/// Every frozen database of an older (or the current) schema opens, migrates and keeps every row
/// with its fields, and its text stays searchable.
#[test]
fn every_older_schema_migrates_without_losing_rows() {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut fixtures: Vec<PathBuf> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("schema-v"))
        .collect();
    fixtures.sort();
    assert!(!fixtures.is_empty(), "there is a fixture for every schema version");
    assert!(
        fixtures.iter().any(|p| p.ends_with(format!("schema-v{SCHEMA_VERSION}.sqlite3"))),
        "the current schema has its fixture"
    );

    type Row = (String, String, String, i64, bool, Option<String>, Option<String>);
    for fixture in fixtures {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("history");
        fs::create_dir_all(&root).unwrap();
        fs::copy(&fixture, root.join("history.sqlite3")).unwrap();
        let before: Vec<Row> = {
            let conn = Connection::open(root.join("history.sqlite3")).unwrap();
            let mut statement = conn
                .prepare("SELECT id, kind, name, created_at_ms, pinned, text, hosted_url FROM entries ORDER BY id")
                .unwrap();
            let rows = statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
                .unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert!(before.len() >= 5, "{} holds a row of every kind", fixture.display());

        let store = HistoryStore::open(HistoryPaths::new(&root), Arc::new(SystemTrash)).unwrap();
        let version: i64 = Connection::open(root.join("history.sqlite3"))
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let page = store.list(&HistoryQuery { limit: 1000, ..HistoryQuery::default() }).unwrap();
        assert_eq!(page.total as usize, before.len(), "{}", fixture.display());
        for (id, kind, name, created, pinned, text, url) in &before {
            let entry = store.get(&EntryId(id.clone())).unwrap();
            assert_eq!(
                (entry.kind.as_str(), &entry.name, entry.created_at_ms, entry.pinned),
                (kind.as_str(), name, *created, *pinned)
            );
            assert_eq!(&entry.text, text);
            assert_eq!(entry.hosted.map(|h| h.url), *url);
        }
        // The fixture's recognised text is still found, through the rebuilt index if need be.
        assert!(store.search("ışık", &HistoryQuery::default()).unwrap().total >= 1, "{}", fixture.display());
    }
}

/// Writes the fixture of the current schema. Run once when a schema version is added:
/// `cargo test -p glimpsfy-history -- --ignored write_schema_fixture`.
#[test]
#[ignore]
fn write_schema_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("history");
    let store = HistoryStore::open(HistoryPaths::new(&root), Arc::new(SystemTrash)).unwrap();
    let lines = RecognizedText {
        image_size: PixelSize { width: 1200, height: 740 },
        lines: vec![
            TextLine {
                text: "IŞIK AYARLARI".into(),
                bounds: TextBox { x: 40.0, y: 30.0, width: 240.0, height: 24.0 },
                words: vec![],
            },
            TextLine {
                text: "İstanbul ofisi, kırmızı düğme".into(),
                bounds: TextBox { x: 40.0, y: 70.0, width: 400.0, height: 24.0 },
                words: vec![],
            },
        ],
    };
    let mut shot = screenshot("Ayarlar — Safari", NOW - 3 * DAY);
    shot.source = EntrySource {
        app_name: Some("Safari".into()),
        bundle_id: Some("com.apple.Safari".into()),
        window_title: Some("Ayarlar".into()),
    };
    shot.text = Some("IŞIK AYARLARI\nİstanbul ofisi, kırmızı düğme".into());
    shot.text_lines = Some(lines);
    shot.scale = Some(2.0);
    shot.display = Some(EntryDisplay { id: 1, name: Some("Built-in Retina Display".into()) });
    let shot = store.add(shot, OwnFile::Bytes { bytes: b"png", extension: "png" }).unwrap();
    store.set_hosted(&shot.id, Some(hosted())).unwrap();

    let mut saved = screenshot("Saved — Preview", NOW - 40 * DAY);
    saved.saved_path = Some("/Users/someone/Pictures/Glimpsfy/Glimpsfy 2026-11-16 at 14.32.08.png".into());
    saved.recognize_text = true;
    let saved = store.add(saved, OwnFile::None).unwrap();
    store.set_pinned(&saved.id, true).unwrap();

    let mut recording = screenshot("Checkout flow — staging", NOW - DAY);
    recording.kind = Some(EntryKind::Recording);
    recording.duration_ms = Some(42_000);
    recording.saved_path = Some("/Users/someone/Movies/Glimpsfy/checkout.mp4".into());
    store.add(recording, OwnFile::None).unwrap();

    let mut text = screenshot("Text from INV-2026-0412.pdf", NOW - 2 * DAY);
    text.kind = Some(EntryKind::TextCopy);
    text.size = None;
    text.text = Some("INVOICE INV-2026-0412\nTotal due €96.00".into());
    store.add(text, OwnFile::None).unwrap();

    store.add(screenshot("Plain", NOW), OwnFile::None).unwrap();
    drop(store);

    let conn = Connection::open(root.join("history.sqlite3")).unwrap();
    let _: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0)).unwrap();
    let _: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0)).unwrap();
    drop(conn);
    let target =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(format!("schema-v{SCHEMA_VERSION}.sqlite3"));
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    assert!(!target.exists(), "a fixture is frozen once and never written again");
    fs::copy(root.join("history.sqlite3"), target).unwrap();
}

/// Search finds Turkish text whatever its case and whichever i it uses, in names and in the
/// recognised text, and never finds the text of an entry that was deleted, cleared or swept.
#[test]
fn search_finds_turkish_text_by_any_i_and_forgets_deleted_entries() {
    let f = fixture(false);
    let add = |name: &str, text: &str, age: i64| {
        let entry = f.store.add(screenshot(name, NOW - age), OwnFile::None).unwrap();
        let lines = RecognizedText {
            image_size: PixelSize { width: 800, height: 600 },
            lines: text
                .lines()
                .enumerate()
                .map(|(i, l)| TextLine {
                    text: l.into(),
                    bounds: TextBox { x: 10.0, y: 30.0 * i as f64, width: 300.0, height: 20.0 },
                    words: vec![],
                })
                .collect(),
        };
        f.store.set_text(&entry.id, Some(text.into()), Some(lines)).unwrap()
    };
    let lights = add("Ekran ayarları", "IŞIK AYARLARI\nParlaklık", 0);
    let keyboard = add("Klavye", "Arka ışıklı klavye", DAY);
    let office = add("İSTANBUL OFİSİ", "Adres", 2 * DAY);
    let invoice = add("Mail", "INVOICE INV-2026-0412", 3 * DAY);
    let red = add("Renkler", "Kırmızı düğme", 10 * DAY);

    let cases: &[(&str, &[&str])] = &[
        ("ışık", &["Ekran ayarları", "Klavye"]),
        ("IŞIK", &["Ekran ayarları", "Klavye"]),
        ("Işık", &["Ekran ayarları", "Klavye"]),
        ("isik", &["Ekran ayarları", "Klavye"]),
        ("İŞİK", &["Ekran ayarları", "Klavye"]),
        ("istanbul", &["İSTANBUL OFİSİ"]),
        ("İstanbul", &["İSTANBUL OFİSİ"]),
        ("ISTANBUL", &["İSTANBUL OFİSİ"]),
        ("ıstanbul ofis", &["İSTANBUL OFİSİ"]),
        ("invoice", &["Mail"]),
        ("İNVOİCE", &["Mail"]),
        ("ınvoıce", &["Mail"]),
        ("inv-2026", &["Mail"]),
        ("KIRMIZI", &["Renkler"]),
        ("kirmizi dug", &["Renkler"]),
        ("parlaklik", &["Ekran ayarları"]),
        ("\"; DROP TABLE entries; --", &[]),
    ];
    for (query, expected) in cases {
        let page = f.store.search(query, &HistoryQuery::default()).unwrap();
        let mut found = ids(&page);
        found.sort();
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(found, expected, "query {query:?}");
        for item in &page.items {
            let found = item.text_match.as_ref().unwrap();
            assert!(!found.highlights.is_empty(), "query {query:?} marks its match");
        }
    }
    // The marks fall on the original letters.
    let page = f.store.search("isik", &HistoryQuery::default()).unwrap();
    let lights_match = page.items.iter().find(|i| i.entry.id == lights.id).unwrap().text_match.clone().unwrap();
    let marked: String = {
        let units: Vec<u16> = lights_match.snippet.encode_utf16().collect();
        let h = lights_match.highlights[0];
        String::from_utf16_lossy(&units[h.start as usize..h.end as usize])
    };
    assert_eq!(marked, "IŞIK");
    assert_eq!(lights_match.boxes.len(), 1);

    // Deleted, cleared and swept entries leave the index.
    let none = Hosts { host: Host::Deletes, calls: Mutex::new(Vec::new()) };
    f.store.delete(std::slice::from_ref(&office.id), &DeleteOptions::default(), &none).unwrap();
    assert_eq!(f.store.search("istanbul", &HistoryQuery::default()).unwrap().total, 0);
    f.store.sweep(NOW, Retention::Days7, SweepOptions::default()).unwrap();
    f.store.set_pinned(&keyboard.id, true).unwrap();
    f.store.clear().unwrap();
    assert_eq!(ids(&f.store.search("ışık", &HistoryQuery::default()).unwrap()), vec!["Klavye".to_string()]);
    for gone in [&invoice, &red, &lights] {
        assert!(f.store.get(&gone.id).is_err());
    }
    let raw: i64 = Connection::open(f.store.paths().database())
        .unwrap()
        .query_row("SELECT count(*) FROM entries_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(raw, 1, "the index holds nothing of deleted entries");
}

/// The crash invariant: a file written for an entry whose row never got committed (or a
/// half-written file) is never listed, counted or searched, and the next start removes it,
/// while every file a row names stays.
#[test]
fn a_crash_between_the_file_and_the_row_leaves_no_entry() {
    let f = fixture(false);
    let kept = add_owned(&f, "kept", NOW, false);
    let paths = f.store.paths().clone();
    // What a crash leaves behind: a capture and a thumbnail written under an id no row has, and
    // a write cut short.
    let orphan = paths.captures().join(format!("{}.png", EntryId::new(NOW)));
    let orphan_thumbnail = paths.thumbnails().join(format!("{}.jpg", EntryId::new(NOW)));
    let half = paths.captures().join(".writing-0190000000000-1.png");
    for path in [&orphan, &orphan_thumbnail, &half] {
        fs::write(path, b"bytes").unwrap();
    }
    // A user's file next to the history is not the history's to remove.
    let outside = paths.root().join("notes.txt");
    fs::write(&outside, b"mine").unwrap();

    let reopened = HistoryStore::open(paths.clone(), f.trash.clone()).unwrap();
    let page = reopened.list(&HistoryQuery::default()).unwrap();
    assert_eq!(ids(&page), vec!["kept".to_string()]);
    assert_eq!(reopened.usage().unwrap().entries, 1);
    assert_eq!(reopened.usage().unwrap().own_bytes, (b"own png".len() + b"jpeg".len()) as u64);
    assert_eq!(reopened.search("png", &HistoryQuery::default()).unwrap().total, 0);

    // A write still in progress (newer than the start) is spared; older leftovers go.
    let started = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let report = reopened.reconcile(Some(started)).unwrap();
    assert!(report.removed_files.is_empty(), "files newer than the start may be writes in progress");
    let report = reopened.reconcile(None).unwrap();
    let mut removed = report.removed_files.clone();
    removed.sort();
    let mut expected = vec![orphan, orphan_thumbnail, half];
    expected.sort();
    assert_eq!(removed, expected);
    assert!(kept.file.as_ref().unwrap().exists() && kept.thumbnail.as_ref().unwrap().exists());
    assert_eq!(fs::read(&outside).unwrap(), b"mine");
}

/// The editor's outputs of one image revise one entry: a later Save names the user's file and
/// drops the history's own copy, a later Copy brings a new own copy and keeps the saved file
/// named, the edit's document is rewritten in place beside the image it draws on, a restart keeps
/// the edit's files, and a delete removes every file of the history's and never the user's.
#[test]
fn an_edit_revised_by_later_outputs_keeps_the_saved_file_and_its_drawings() {
    let f = fixture(false);
    let paths = f.store.paths().clone();

    // Copy: the first output adds the entry with its own copy and the edit.
    let document = f.store.write_edit(b"{\"objects\":[1]}", Some(b"base png"), None).unwrap();
    let base = HistoryPaths::edit_base(&document);
    let inbox = f.user.join("inbox.png");
    fs::write(&inbox, b"copied png").unwrap();
    let mut new = screenshot("edited", NOW);
    new.annotations = Some(document.clone());
    let entry = f.store.add(new, OwnFile::Move { from: &inbox }).unwrap();
    let entry = f.store.set_thumbnail(&entry.id, &thumbnail()).unwrap();
    let (first_copy, first_thumbnail) = (entry.file.clone().unwrap(), entry.thumbnail.clone().unwrap());

    // Save: the same entry names the user's file; the own copy and the stale thumbnail go.
    let saved = f.user.join("edited.png");
    fs::write(&saved, b"saved png").unwrap();
    let rewritten = f.store.write_edit(b"{\"objects\":[1,2]}", None, Some(&document)).unwrap();
    assert_eq!(rewritten, document, "an edit's document is rewritten in place");
    let revision = Revision {
        own: OwnFile::None,
        saved_path: Some(saved.clone()),
        size: Some(PixelSize { width: 80, height: 40 }),
        scale: None,
        file_bytes: Some(9),
        annotations: Some(rewritten),
    };
    let entry = f.store.revise(&entry.id, revision).unwrap();
    assert_eq!(f.store.list(&HistoryQuery::default()).unwrap().total, 1);
    assert_eq!(entry.saved_path.as_deref(), Some(saved.as_path()));
    assert!(entry.file.is_none() && entry.thumbnail.is_none());
    assert!(!first_copy.exists() && !first_thumbnail.exists());
    assert_eq!(fs::read(&document).unwrap(), b"{\"objects\":[1,2]}");
    assert_eq!(fs::read(&base).unwrap(), b"base png");

    // Copy again: a new own copy, and the saved file stays named.
    let inbox = f.user.join("inbox-2.png");
    fs::write(&inbox, b"copied again").unwrap();
    let revision = Revision {
        own: OwnFile::Move { from: &inbox },
        saved_path: None,
        size: None,
        scale: None,
        file_bytes: None,
        annotations: None,
    };
    let entry = f.store.revise(&entry.id, revision).unwrap();
    let own = entry.file.clone().unwrap();
    assert_eq!(fs::read(&own).unwrap(), b"copied again");
    assert!(!inbox.exists());
    assert_eq!(entry.saved_path.as_deref(), Some(saved.as_path()));
    assert_eq!(entry.annotations.as_deref(), Some(document.as_path()));

    // A restart keeps the edit's files.
    let reopened = HistoryStore::open(paths, f.trash.clone()).unwrap();
    assert!(reopened.reconcile(None).unwrap().removed_files.is_empty());
    assert!(document.exists() && base.exists() && own.exists());

    // A delete removes the history's files and leaves the user's.
    let none = Hosts { host: Host::Deletes, calls: Mutex::new(Vec::new()) };
    reopened.delete(std::slice::from_ref(&entry.id), &DeleteOptions::default(), &none).unwrap();
    assert!(!document.exists() && !base.exists() && !own.exists());
    assert_eq!(fs::read(&saved).unwrap(), b"saved png");
}
