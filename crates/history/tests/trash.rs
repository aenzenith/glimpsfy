//! The system Trash on Linux, for real: a trashed file must lie in the freedesktop.org Trash with
//! the record that puts it back, and a file the Trash cannot take must stay where it is. The Trash
//! is a folder of the test's own (`XDG_DATA_HOME`), never the user's.
#![cfg(target_os = "linux")]

use std::fs;
use std::io;

use glimpsfy_history::{SystemTrash, Trash};

#[test]
fn a_file_reaches_the_trash_with_its_record_or_stays_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let data = root.join("data");
    // This is the only test of the binary, so no other thread reads the environment meanwhile.
    std::env::set_var("XDG_DATA_HOME", &data);
    let user = root.join("user");
    fs::create_dir_all(&user).unwrap();

    // A trashed file is in the Trash whole, beside the record a file manager restores it from.
    let saved = user.join("Shot 1 ç.png");
    fs::write(&saved, b"first").unwrap();
    SystemTrash.trash(&saved).expect("the file is trashed");
    assert!(!saved.exists());
    assert_eq!(fs::read(data.join("Trash/files/Shot 1 ç.png")).unwrap(), b"first");
    let record = fs::read_to_string(data.join("Trash/info/Shot 1 ç.png.trashinfo")).unwrap();
    let mut lines = record.lines();
    assert_eq!(lines.next(), Some("[Trash Info]"));
    assert_eq!(lines.next(), Some(format!("Path={}/Shot%201%20%C3%A7.png", user.display()).as_str()));
    assert!(lines.next().is_some_and(|line| line.starts_with("DeletionDate=2")), "{record}");

    // A second file of the same name does not replace the first one in the Trash.
    fs::write(&saved, b"second").unwrap();
    SystemTrash.trash(&saved).expect("the second file is trashed");
    assert_eq!(fs::read(data.join("Trash/files/Shot 1 ç.png")).unwrap(), b"first");
    assert_eq!(fs::read(data.join("Trash/files/Shot 1 ç.png.2")).unwrap(), b"second");

    let missing = SystemTrash.trash(&saved).expect_err("nothing is left to trash");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);

    // A Trash that cannot be written (its folder lies below a plain file here) refuses the file,
    // and the file is still there, untouched.
    fs::write(root.join("blocked"), b"").unwrap();
    std::env::set_var("XDG_DATA_HOME", root.join("blocked").join("data"));
    let kept = user.join("Kept.png");
    fs::write(&kept, b"keep me").unwrap();
    SystemTrash.trash(&kept).expect_err("the Trash refused");
    assert_eq!(fs::read(&kept).unwrap(), b"keep me");
}
