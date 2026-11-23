//! The command line and the `glimpsfy://` URL scheme: what a launch asks for, the answers that
//! need no app (help, version, a usage error, `mcp`), the detached launch a verb hands its request
//! to, and the four captures a URL may start.
//!
//! The URL scheme is a security boundary: any web page can open a `glimpsfy://` address, so it
//! only starts a capture whose selection the user still makes. Nothing a URL says can upload,
//! read or write a file, record, or run a command; `parse_url` returns a `UrlCapture`, which has no
//! other value. The command line is a local process with the user's own rights and is no boundary.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tauri::AppHandle;

use crate::i18n;
use crate::settings::Language;
use crate::shortcuts::{self, Action, ACTIONS};
use crate::trace;

/// What a launch asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    /// A plain start, or a second one that only wakes the running app.
    App,
    /// A shortcut's action, from a verb (`capture --area`) or `--action <name>`.
    Action(Action),
    /// One of the captures a URL may start.
    Url(UrlCapture),
    /// A URL Glimpsfy refuses; it is logged and nothing else happens.
    Refused,
    Help,
    Version,
    /// `glimpsfy mcp`, the agents' stdio bridge (D34).
    Mcp,
    Usage(Usage),
}

/// A command line Glimpsfy cannot read.
#[derive(Debug, PartialEq, Eq)]
pub enum Usage {
    /// A verb or option that does not exist.
    Unknown(String),
    /// A verb with no option, or with more than one; it names the verb.
    OneOption(&'static str),
}

/// A verb and the shortcut each of its options runs. `--delayed` is no option of its own: it
/// turns `capture --area` into the delayed capture.
struct Verb {
    name: &'static str,
    options: &'static [(&'static str, Action)],
}

static VERBS: [Verb; 3] = [
    Verb {
        name: "capture",
        options: &[
            ("--area", Action::CaptureArea),
            ("--window", Action::CaptureWindow),
            ("--display", Action::CaptureDisplay),
            ("--all-displays", Action::CaptureAllDisplays),
            ("--last-area", Action::RepeatLastArea),
            ("--text", Action::CopyText),
        ],
    },
    Verb {
        name: "record",
        options: &[
            ("--area", Action::RecordScreen),
            ("--display", Action::RecordDisplay),
            ("--pause", Action::PauseRecording),
            ("--mute", Action::MuteMicrophone),
            ("--discard", Action::DiscardRecording),
        ],
    },
    Verb { name: "open", options: &[("--history", Action::OpenHistory)] },
];

const DELAYED: &str = "--delayed";

/// Set on the detached launch a verb starts, so that launch runs the request instead of
/// detaching again.
const CHILD: &str = "GLIMPSFY_CLI_CHILD";

/// The captures a URL may start. Each lets the user choose what is captured, so a web page can
/// never take the screen without the user's selection, and none of them leads anywhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlCapture {
    Area,
    AreaDelayed,
    Window,
    Text,
}

impl UrlCapture {
    pub fn action(self) -> Action {
        match self {
            UrlCapture::Area => Action::CaptureArea,
            UrlCapture::AreaDelayed => Action::CaptureAreaDelayed,
            UrlCapture::Window => Action::CaptureWindow,
            UrlCapture::Text => Action::CopyText,
        }
    }
}

/// The longest URL that is read at all; the four that pass are far shorter.
const URL_MAX: usize = 64;

/// The capture a `glimpsfy://` URL asks for. Only the four exact addresses pass
/// (`glimpsfy://capture/area`, `…/area?delayed`, `…/window`, `…/text`), so upper case, a user,
/// a port, a fragment, percent-encoding, dot segments, more path and any other query are refused
/// without being decoded.
pub fn parse_url(url: &str) -> Option<UrlCapture> {
    if url.len() > URL_MAX {
        return None;
    }
    let rest = url.strip_prefix("glimpsfy://")?;
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let capture = match path {
        "capture/area" => UrlCapture::Area,
        "capture/window" => UrlCapture::Window,
        "capture/text" => UrlCapture::Text,
        _ => return None,
    };
    match (capture, query) {
        (capture, None) => Some(capture),
        (UrlCapture::Area, Some("delayed")) => Some(UrlCapture::AreaDelayed),
        _ => None,
    }
}

/// Whether an argument is meant as a URL of the scheme, in any case.
fn is_url(argument: &str) -> bool {
    argument.as_bytes().get(..9).is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"glimpsfy:"))
}

/// `parse_url`, with a refusal written to the log.
fn accept_url(url: &str) -> Option<UrlCapture> {
    let capture = parse_url(url);
    if capture.is_none() {
        // Cut and escaped, so a long or odd address cannot flood or forge log lines.
        let shown: String = url.chars().take(URL_MAX + 16).collect();
        trace::log(format!("[glimpsfy] URL refused: {shown:?}"));
    }
    capture
}

/// What `arguments` (the whole argv, the program first) asks for.
pub fn parse(arguments: &[String]) -> Invocation {
    let rest = arguments.get(1..).unwrap_or_default();
    // `mcp` comes first, as it did before the other commands existed: an agent's configuration
    // starts it, and it never reaches Tauri.
    if rest.first().is_some_and(|first| first == "mcp") {
        return Invocation::Mcp;
    }
    // The system starts `glimpsfy <url>` and nothing else, so a URL with anything beside it was
    // put together by someone else (an injected `--action`) and is refused whole.
    if rest.iter().any(|argument| is_url(argument)) {
        return match rest {
            [url] => accept_url(url).map_or(Invocation::Refused, Invocation::Url),
            _ => {
                trace::log("[glimpsfy] URL refused: it came with other arguments");
                Invocation::Refused
            }
        };
    }
    // `--action <name>` reads as it always did, anywhere on the line; an unknown name is logged
    // and only wakes the app.
    if let Some(name) = action_name(rest) {
        return match name.and_then(|name| ACTIONS.into_iter().find(|action| action.cli_name() == name)) {
            Some(action) => Invocation::Action(action),
            None => {
                trace::log(format!("[glimpsfy] --action {}: no such action", name.unwrap_or_default()));
                Invocation::App
            }
        };
    }
    let Some((first, options)) = rest.split_first() else {
        return Invocation::App;
    };
    match first.as_str() {
        "help" | "-h" | "--help" => match options.first() {
            None => Invocation::Help,
            Some(extra) => Invocation::Usage(Usage::Unknown(extra.clone())),
        },
        "version" | "-V" | "--version" => match options.first() {
            None => Invocation::Version,
            Some(extra) => Invocation::Usage(Usage::Unknown(extra.clone())),
        },
        // `run` is kept for workflows (B13) and is unknown until then.
        name => match VERBS.iter().find(|verb| verb.name == name) {
            Some(verb) => parse_verb(verb, options),
            None => Invocation::Usage(Usage::Unknown(first.clone())),
        },
    }
}

/// The name after `--action` or in `--action=<name>` (the action's name in kebab-case): `None`
/// when there is no such option, `Some(None)` when it ends the line without a name. Desktops
/// whose keys Glimpsfy cannot hear (Wayland without the GlobalShortcuts portal) bind
/// `glimpsfy --action capture-area`, so it keeps its old form beside the verbs.
fn action_name(rest: &[String]) -> Option<Option<&str>> {
    let mut arguments = rest.iter();
    loop {
        let argument = arguments.next()?;
        if argument == "--action" {
            return Some(arguments.next().map(String::as_str));
        }
        if let Some(name) = argument.strip_prefix("--action=") {
            return Some(Some(name));
        }
    }
}

fn parse_verb(verb: &'static Verb, options: &[String]) -> Invocation {
    let mut chosen = Vec::new();
    let mut delayed = false;
    for option in options {
        if verb.name == "capture" && option == DELAYED {
            delayed = true;
            continue;
        }
        match verb.options.iter().find(|(flag, _)| flag == option) {
            Some((_, action)) => chosen.push(*action),
            None => return Invocation::Usage(Usage::Unknown(option.clone())),
        }
    }
    match (chosen.as_slice(), delayed) {
        ([Action::CaptureArea], true) => Invocation::Action(Action::CaptureAreaDelayed),
        ([action], false) => Invocation::Action(*action),
        _ => Invocation::Usage(Usage::OneOption(verb.name)),
    }
}

/// A text of the catalogs in the system's language; no settings are read before the app runs.
fn say(key: &str, params: &[(&str, &str)]) -> String {
    i18n::text(Language::System, key, params)
}

impl Usage {
    fn message(&self) -> String {
        match self {
            Usage::Unknown(name) => say("cli.unknown", &[("name", name)]),
            Usage::OneOption(name) => {
                let options = VERBS
                    .iter()
                    .find(|verb| verb.name == *name)
                    .map(|verb| {
                        verb.options.iter().map(|(flag, action)| option_text(flag, *action)).collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
                    .join(", ");
                say("cli.oneOption", &[("verb", name), ("options", &options)])
            }
        }
    }
}

fn option_text(flag: &str, action: Action) -> String {
    if action == Action::CaptureArea {
        format!("{flag} [{DELAYED}]")
    } else {
        flag.to_string()
    }
}

/// `glimpsfy help`: every command with the name of the shortcut it stands for.
fn help() -> String {
    let mut commands: Vec<(String, Option<Action>)> = Vec::new();
    for verb in &VERBS {
        for (flag, action) in verb.options {
            commands.push((format!("glimpsfy {} {flag}", verb.name), Some(*action)));
            if *action == Action::CaptureArea {
                commands.push((format!("glimpsfy {} {flag} {DELAYED}", verb.name), Some(Action::CaptureAreaDelayed)));
            }
        }
    }
    let mut plain = vec!["glimpsfy --action <name>"];
    if cfg!(target_os = "macos") {
        plain.push("glimpsfy mcp");
    }
    plain.extend(["glimpsfy help", "glimpsfy version"]);
    commands.extend(plain.into_iter().map(|command| (command.to_string(), None)));

    let width = commands.iter().map(|(command, _)| command.len()).max().unwrap_or_default();
    let mut text = format!("Glimpsfy {}\n{}\n\n", env!("CARGO_PKG_VERSION"), say("cli.about", &[]));
    for (command, action) in commands {
        let label = action
            .and_then(|action| serde_json::to_value(action).ok())
            .and_then(|name| name.as_str().map(|name| say(&format!("settings.shortcuts.action.{name}"), &[])));
        match label {
            Some(label) => text.push_str(&format!("  {command:<width$}   {label}\n")),
            None => text.push_str(&format!("  {command}\n")),
        }
    }
    text
}

/// The first thing `run()` does. Help, version, a usage error and `mcp` end here, before Tauri
/// and the single-instance plugin start. A verb starts Glimpsfy again, detached, with the same
/// arguments and returns 0 at once, so a script never waits for a capture; 0 says the request
/// was given to Glimpsfy, not that a capture was taken. Anything else returns, and this process
/// becomes the app or hands its arguments to the running one.
pub fn early() {
    let arguments: Vec<String> = std::env::args_os().map(|argument| argument.to_string_lossy().into_owned()).collect();
    let code = match parse(&arguments) {
        Invocation::Help => {
            write(Stream::Out, &help());
            0
        }
        Invocation::Version => {
            write(Stream::Out, &format!("Glimpsfy {}\n", env!("CARGO_PKG_VERSION")));
            0
        }
        Invocation::Usage(usage) => {
            write(Stream::Err, &format!("{}\n{}\n", usage.message(), say("cli.hint", &[])));
            2
        }
        Invocation::Mcp => mcp(),
        Invocation::Action(_) if names_verb(&arguments) && std::env::var_os(CHILD).is_none() => {
            match detach(&arguments) {
                Ok(()) => 0,
                Err(reason) => {
                    write(Stream::Err, &format!("{}\n", say("cli.startFailed", &[("reason", &reason)])));
                    1
                }
            }
        }
        _ => return,
    };
    std::process::exit(code);
}

/// Whether the line starts with one of the verbs, rather than `--action`, which runs in this
/// process as it always did.
fn names_verb(arguments: &[String]) -> bool {
    arguments.get(1).is_some_and(|first| VERBS.iter().any(|verb| verb.name == first))
}

fn mcp() -> i32 {
    #[cfg(target_os = "macos")]
    {
        glimpsfy_mcp::bridge::run(cfg!(debug_assertions))
    }
    #[cfg(not(target_os = "macos"))]
    {
        write(Stream::Err, &format!("{}\n", say("cli.noMcp", &[])));
        1
    }
}

enum Stream {
    Out,
    Err,
}

/// Writes to the terminal. A closed pipe (`glimpsfy help | head -1`) is no failure of Glimpsfy's,
/// so a failed write is dropped instead of panicking as `print!` would.
fn write(stream: Stream, text: &str) {
    use std::io::Write;
    attach_console();
    let _ = match stream {
        Stream::Out => std::io::stdout().lock().write_all(text.as_bytes()),
        Stream::Err => std::io::stderr().lock().write_all(text.as_bytes()),
    };
}

/// A Windows release is a GUI program, which has no console of its own; it writes into the one
/// it was run from.
fn attach_console() {
    #[cfg(all(target_os = "windows", not(debug_assertions)))]
    {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        // SAFETY: the call takes no pointers; without a parent console it fails and changes nothing.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Starts Glimpsfy again with the same arguments and `CHILD` set, without waiting for it.
fn detach(arguments: &[String]) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let rest = arguments.get(1..).unwrap_or_default();

    // A process the terminal starts asks for screen recording in the terminal's name; one Launch
    // Services starts asks in Glimpsfy's. `-n` starts a new process even while Glimpsfy runs, so
    // it can hand its arguments over, and `-g` leaves the frontmost app in front.
    #[cfg(target_os = "macos")]
    if let Some(bundle) = crate::update::install_path() {
        let status = Command::new("/usr/bin/open")
            .args(["-n", "-g", "-a"])
            .arg(&bundle)
            .arg("--env")
            .arg(format!("{CHILD}=1"))
            .arg("--args")
            .args(rest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| error.to_string())?;
        return if status.success() { Ok(()) } else { Err(format!("open: {status}")) };
    }

    let mut command = Command::new(launcher()?);
    command.args(rest).env(CHILD, "1").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // Its own process group, so Ctrl-C in the terminal and the terminal's hang-up do not end it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(drop).map_err(|error| error.to_string())
}

/// The program to start again: an AppImage's own file, because the place its runtime mounts it
/// at goes away when this process ends, and this executable everywhere else.
fn launcher() -> Result<std::path::PathBuf, String> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("APPIMAGE").filter(|value| !value.is_empty()) {
        return Ok(appimage.into());
    }
    std::env::current_exe().map_err(|error| error.to_string())
}

/// How long a request of the first launch waits for the windows made at start to load.
const LAUNCH_ACTION_AFTER: Duration = Duration::from_millis(1200);

/// When the app started, as far as a request is concerned; `run_launch` fixes it in `setup`.
fn started() -> Instant {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    *STARTED.get_or_init(Instant::now)
}

/// Runs `action` on the main thread, as a shortcut would, once the windows made at start have
/// loaded: at once in an app that has run for a while, after the rest of the wait in one that
/// has just started (a URL that started it arrives as an event right after `setup`).
pub fn run_when_ready(app: &AppHandle, action: Action) {
    let wait = LAUNCH_ACTION_AFTER.saturating_sub(started().elapsed());
    let app = app.clone();
    let run = move || {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || shortcuts::run(&handle, action, Instant::now()));
    };
    if wait.is_zero() {
        run();
    } else {
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            run();
        });
    }
}

/// Starts the capture a URL asked for.
pub fn open_url(app: &AppHandle, capture: UrlCapture) {
    run_when_ready(app, capture.action());
}

/// In `setup`: runs what this launch's command line asks for once the app is up. A launch while
/// Glimpsfy already runs hands its arguments to the running one instead (`lib.rs`). On macOS a
/// URL arrives as an Apple Event through the deep-link plugin, never on the command line of an
/// installed app; an AppImage registers the scheme itself, since no package installed it.
pub fn run_launch(app: &AppHandle) {
    started();
    let arguments: Vec<String> = std::env::args_os().map(|argument| argument.to_string_lossy().into_owned()).collect();
    match parse(&arguments) {
        Invocation::Action(action) => run_when_ready(app, action),
        Invocation::Url(capture) => open_url(app, capture),
        _ => {}
    }
    #[cfg(target_os = "macos")]
    follow_urls(app);
    #[cfg(target_os = "linux")]
    register_appimage(app);
}

/// Reads the URL that started the app once, in case the system delivered it before `setup`, and
/// every later one as it comes. Both run on the main thread, so none is seen twice.
#[cfg(target_os = "macos")]
fn follow_urls(app: &AppHandle) {
    use tauri_plugin_deep_link::DeepLinkExt;
    let links = app.deep_link();
    if let Ok(Some(urls)) = links.get_current() {
        open_urls(app, urls.iter().map(ToString::to_string).collect());
    }
    let handle = app.clone();
    links.on_open_url(move |event| open_urls(&handle, event.urls().iter().map(ToString::to_string).collect()));
}

/// One link is one request; several at once were not one click on a link and are refused.
#[cfg(target_os = "macos")]
fn open_urls(app: &AppHandle, urls: Vec<String>) {
    match urls.as_slice() {
        [url] => {
            if let Some(capture) = accept_url(url) {
                open_url(app, capture);
            }
        }
        _ => trace::log(format!("[glimpsfy] URL refused: {} at once", urls.len())),
    }
}

/// A release AppImage writes its own handler for the scheme at every start, so it follows the
/// file when the user moves it. A development run registers nothing and never takes the scheme
/// from an installed copy.
#[cfg(target_os = "linux")]
fn register_appimage(app: &AppHandle) {
    if cfg!(debug_assertions) || std::env::var_os("APPIMAGE").is_none() {
        return;
    }
    let app = app.clone();
    // It writes a desktop file and runs `xdg-mime`, which the main thread does not wait for.
    std::thread::spawn(move || {
        use tauri_plugin_deep_link::DeepLinkExt;
        if let Err(error) = app.deep_link().register_all() {
            trace::log(format!("[glimpsfy] URL scheme: {error}"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(arguments: &[&str]) -> Vec<String> {
        std::iter::once("glimpsfy").chain(arguments.iter().copied()).map(str::to_string).collect()
    }

    /// Any web page can open a `glimpsfy://` address, so the scheme must start nothing but the
    /// four captures that leave the selection to the user: no upload, no file read, no recording,
    /// no command, however the address is dressed up.
    #[test]
    fn the_url_scheme_starts_captures_only() {
        let accepted = [
            ("glimpsfy://capture/area", Action::CaptureArea),
            ("glimpsfy://capture/area?delayed", Action::CaptureAreaDelayed),
            ("glimpsfy://capture/window", Action::CaptureWindow),
            ("glimpsfy://capture/text", Action::CopyText),
        ];
        for (url, action) in accepted {
            assert_eq!(parse_url(url).map(UrlCapture::action), Some(action), "{url}");
            assert_eq!(parse(&argv(&[url])), Invocation::Url(parse_url(url).unwrap()), "{url}");
        }

        let long = format!("glimpsfy://capture/area?delayed{}", "&".repeat(65 - 31));
        assert_eq!(long.len(), 65);
        let refused = [
            "glimpsfy://upload",
            "glimpsfy://upload/area",
            "glimpsfy://capture/upload",
            "glimpsfy://read?path=/etc/passwd",
            "glimpsfy://capture/read",
            "glimpsfy://run/workflow",
            "glimpsfy://exec/ls",
            "glimpsfy://record/area",
            "glimpsfy://capture/record",
            "glimpsfy://mcp",
            "glimpsfy://open/history",
            "glimpsfy://shoot/area",
            "glimpsfy://capture/display",
            "glimpsfy://capture/all-displays",
            "glimpsfy://capture/last-area",
            "glimpsfy://capture/area?action=upload",
            "glimpsfy://capture/area?path=/tmp/x.png",
            "glimpsfy://capture/area?delayed&action=upload",
            "glimpsfy://capture/area?delayed=1",
            "glimpsfy://capture/window?delayed",
            "glimpsfy://capture/%61rea",
            "glimpsfy://capture%2farea",
            "glimpsfy://capture/../area",
            "glimpsfy://capture/area/..",
            "glimpsfy://capture/./area",
            "glimpsfy://capture/area/",
            "glimpsfy://capture/area/upload",
            "glimpsfy://capture:80/area",
            "glimpsfy://user@capture/area",
            "glimpsfy://capture/area#upload",
            "GLIMPSFY://capture/area",
            "glimpsfy://Capture/area",
            "glimpsfy://capture/AREA",
            "glimpsfy:capture/area",
            "glimpsfy:///capture/area",
            "glimpsfy://capture/area\0",
            "glimpsfy://capture/area\0upload",
            " glimpsfy://capture/area",
            "glimpsfy://capture/area ",
            "glimpsfy://capture/area\n",
            "glimpsfy://",
            "",
            &long,
        ];
        for url in refused {
            assert_eq!(parse_url(url), None, "{url:?}");
            if !url.is_empty() && is_url(url) {
                assert_eq!(parse(&argv(&[url])), Invocation::Refused, "{url:?}");
            }
        }

        // Every shortcut's name, in every shape an address could give it: only the four pass.
        let mut passed = std::collections::BTreeSet::new();
        for action in ACTIONS {
            let name = action.cli_name();
            let (head, tail) = name.split_once('-').unwrap_or((name, ""));
            for path in
                [name.to_string(), format!("capture/{name}"), format!("{head}/{tail}"), format!("capture/{tail}")]
            {
                for url in [format!("glimpsfy://{path}"), format!("glimpsfy://{path}?delayed")] {
                    if let Some(capture) = parse_url(&url) {
                        assert!(accepted.iter().any(|(_, action)| *action == capture.action()));
                        passed.insert(url);
                    }
                }
            }
        }
        let expected: std::collections::BTreeSet<String> = accepted.iter().map(|(url, _)| url.to_string()).collect();
        assert_eq!(passed, expected);

        // The system hands over the URL alone; anything beside it refuses the whole line.
        for line in [
            &["glimpsfy://capture/area", "--action", "record-screen"][..],
            &["--action", "record-screen", "glimpsfy://capture/area"],
            &["--action=open-history", "glimpsfy://capture/area"],
            &["capture", "--area", "glimpsfy://capture/area"],
            &["glimpsfy://capture/area", "glimpsfy://capture/window"],
            &["glimpsfy://upload", "--action", "capture-area"],
            &["GLIMPSFY://capture/area", "--action", "record-screen"],
        ] {
            assert_eq!(parse(&argv(line)), Invocation::Refused, "{line:?}");
        }
    }
}
