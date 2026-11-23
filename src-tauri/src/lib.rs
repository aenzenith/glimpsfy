//! Glimpsfy's core: the menu bar app, its windows, shortcuts and commands.
//!
//! Layout (see `docs/ARCHITECTURE.md` for the ownership map):
//!
//! - `commands/`: the `#[tauri::command]` API, one file per area, one handler list.
//! - `session`: the capture flow from the shortcut to the closed overlay.
//! - `scroll`: the scrolling capture: the selection's live stream, the stitcher and auto scroll.
//! - `capture_log`: the log of slow and failed still captures, in release builds too.
//! - `frames`: the URI scheme that serves frozen frames to the overlays.
//! - `windows`: the overlay pool, the toast and the ordinary windows.
//! - `output`: crop, PNG, clipboard and files.
//! - `colors`: the colour picker's recent colours and format (`colors.json`).
//! - `settings`, `shortcuts`, `tray`, `permissions`: what their names say.
//! - `macos`: AppKit glue (panels, levels, pasteboard, display changes).
//! - `trace`, `dev`: latency marks and the dev-only measurement switches.

mod calendar;
mod capture_log;
mod colors;
pub mod commands;
mod dev;
#[cfg(debug_assertions)]
mod driver;
pub mod events;
mod frames;
mod i18n;
#[cfg(target_os = "macos")]
mod macos;
mod output;
mod paths;
mod permissions;
mod pins;
mod popover;
mod recording;
mod scroll;
mod send;
mod session;
mod settings;
mod shortcuts;
mod trace;
mod tray;
mod update;
mod windows;

use std::sync::Arc;

use glimpsfy_capture::CaptureBackend;
use tauri::Manager;

/// Everything the commands share, managed by Tauri.
pub struct AppState {
    pub backend: Arc<dyn CaptureBackend>,
    pub settings: settings::SettingsStore,
    pub sessions: session::Sessions,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The screen recording probe (`permissions::probe`) answers and exits before anything starts.
    if let Some(code) = permissions::probe() {
        std::process::exit(code);
    }
    tauri::Builder::default()
        // A second launch (a login item racing a manual start) does nothing, unless the menu bar
        // icon is hidden: then it opens Settings, the only way back to them.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| popover::second_launch(app)))
        .plugin(shortcuts::plugin())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .register_asynchronous_uri_scheme_protocol(frames::SCHEME, frames::handle)
        .register_asynchronous_uri_scheme_protocol(commands::history::SCHEME, commands::history::serve)
        .register_asynchronous_uri_scheme_protocol(recording::media::SCHEME, recording::media::serve)
        .invoke_handler(commands::handler())
        .setup(|app| {
            // A menu bar app: no Dock icon, no app menu.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            #[cfg(target_os = "macos")]
            macos::guard_activation();

            let config_dir = paths::config_dir(app.handle())?;
            let store = settings::SettingsStore::open(config_dir.join("settings.json"));
            let backend = glimpsfy_capture::new_backend();
            app.manage(AppState { backend: backend.clone(), settings: store, sessions: session::Sessions::default() });

            let handle = app.handle().clone();
            shortcuts::apply(&handle, &app.state::<AppState>().settings.get().shortcuts);
            tray::create(&handle)?;
            windows::setup(&handle);
            // The history opens its database on its own thread.
            commands::history::setup(&handle);
            send::setup(&handle);
            // Uploads keep the handle for deletions the history asks for.
            commands::upload::setup(&handle);
            // Screen recording: the recorder, its windows and the trim window's library.
            recording::setup(&handle);
            // App shell: the icon as Settings has it, the popover, no pins from an earlier run,
            // and the first-run window when it is due.
            tray::set_visible(&handle, app.state::<AppState>().settings.get().general.show_menu_bar_icon);
            popover::setup(&handle);
            pins::clear_cache(&handle);
            permissions::at_launch(&handle);

            // The login item points at whichever binary registered it; it is written again from
            // the installed app, so it never starts a stale or dev build, and an agent written
            // before it named the bundle gets the name now (`commands/settings.rs`).
            if update::is_bundled() && app.state::<AppState>().settings.get().general.start_at_login {
                if let Err(error) = commands::settings::login_item::set(&handle, true) {
                    trace::log(format!("[glimpsfy] login item: {error}"));
                }
            }
            // Glimpsfy's own updates: asked for once a day while Settings allows it.
            app.manage(update::Update::new(&handle));
            update::Update::start(&handle);

            // ScreenCaptureKit's first call is slow; pay it now, off the main thread.
            std::thread::spawn(move || backend.prepare());

            // The dev-only driver, when `GLIMPSFY_DRIVER` asks for it.
            #[cfg(debug_assertions)]
            driver::setup(&handle);

            let runs = dev::options().autocapture;
            if runs > 0 {
                session::autocapture(handle.clone(), runs);
            }
            trace::log(format!(
                "[glimpsfy] ready · frame format {:?}{}",
                frames::FrameFormat::current(),
                if dev::options().synthetic { " · synthetic frames" } else { "" }
            ));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Glimpsfy")
        .run(|app, event| {
            // Every quit (the menu, Settings, the dev driver) comes through here. A take still
            // running would be cut at its last fragment and miss the history, so it ends first.
            // A restart cannot be held; Tauri ignores `prevent_exit` for it.
            if let tauri::RunEvent::ExitRequested { code, api, .. } = &event {
                // A transcription is cancelled first, a restart included: its model has to leave
                // the GPU before the process ends.
                recording::transcript::end_for_exit(app);
                if *code != Some(tauri::RESTART_EXIT_CODE) && recording::hold_exit(app) {
                    api.prevent_exit();
                }
            }
        });
}
