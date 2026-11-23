//! The command API the pages call, one file per area, all registered in `handler` below.
//!
//! A command's name starts with its area (`capture_start`, `output_copy`…), and its TypeScript
//! wrapper in `src/lib/api/<area>.ts` has the same name, so a search finds both sides. Every command returns
//! `CommandResult<T>` (or a plain value when it cannot fail), and a contract whose
//! implementation is not there yet returns `ErrorCode::NotImplemented`, never a panic.
//!
//! Adding a command: write it in its area's file, add it to the list below, add its wrapper to
//! the area's `.ts` file, and add a row to the command table in `docs/ARCHITECTURE.md`.

pub mod capture;
pub mod error;
pub mod history;
pub mod ocr;
pub mod output;
pub mod permissions;
pub mod record;
pub mod render;
pub mod send;
pub mod settings;
pub mod subject;
pub mod transcript;
pub mod update;
pub mod upload;
pub mod windows;

/// The one list of commands.
pub fn handler() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        // capture
        capture::capture_start,
        capture::capture_current,
        capture::capture_close,
        capture::overlay_ready,
        capture::overlay_painted,
        capture::capture_displays,
        capture::capture_windows,
        capture::overlay_failed,
        capture::capture_pick_window,
        capture::capture_set_beautify,
        capture::capture_set_delay,
        capture::capture_delay,
        capture::capture_cancel_delay,
        // output
        output::output_copy,
        output::output_save,
        output::output_save_as,
        output::output_reveal,
        // copy path and send to app
        send::send_list_targets,
        send::send_pick_app,
        send::send_copy_path,
        send::send_to_app,
        send::send_cancel,
        render::send_rendered,
        // rendered output and the editor (annotation)
        render::output_copy_rendered,
        render::output_save_rendered,
        render::output_save_as_rendered,
        render::output_stage_rendered,
        render::upload_capture_rendered,
        render::editor_open,
        render::editor_open_capture,
        render::editor_combine,
        render::editor_current,
        render::editor_image,
        render::editor_close,
        render::editor_set_document,
        // ocr
        ocr::ocr_recognize,
        ocr::ocr_recognize_pixels,
        ocr::ocr_copy_text,
        ocr::ocr_copy,
        ocr::ocr_detect_codes,
        ocr::ocr_detect_codes_pixels,
        ocr::ocr_cancel,
        ocr::ocr_languages,
        ocr::ocr_engines,
        ocr::ocr_models,
        ocr::ocr_model_download,
        ocr::ocr_model_remove,
        ocr::ocr_detect_entities,
        // subject lifting (annotation)
        subject::subject_find,
        subject::subject_cut,
        subject::subject_copy,
        subject::subject_copy_rendered,
        subject::subject_cancel,
        // upload
        upload::upload_providers,
        upload::upload_capture,
        upload::upload_file,
        upload::upload_cancel,
        upload::upload_delete,
        upload::upload_set_secret,
        upload::upload_remove_secret,
        upload::upload_test,
        upload::upload_validate,
        upload::upload_preflight,
        upload::upload_confirm_provider,
        upload::upload_secret_status,
        upload::upload_import_sxcu,
        // history
        history::history_list,
        history::history_get,
        history::history_search,
        history::history_delete,
        history::history_set_pinned,
        history::history_rename,
        history::history_clear,
        history::history_usage,
        history::history_record_upload,
        history::history_copy,
        history::history_open_in_editor,
        history::history_save_as,
        history::history_delete_hosted,
        history::history_forget_link,
        history::history_export,
        // record
        record::record_setup,
        record::record_setup_state,
        record::record_setup_update,
        record::record_setup_close,
        record::record_bar_fit,
        record::record_start,
        record::record_state,
        record::record_countdown_skip,
        record::record_countdown_cancel,
        record::record_pause,
        record::record_resume,
        record::record_stop,
        record::record_discard,
        record::record_keep,
        record::record_set_microphone_muted,
        record::record_devices,
        record::record_meter,
        record::record_bubble_state,
        record::record_bubble_set,
        record::record_canvas_state,
        record::record_present,
        record::record_present_state,
        record::record_rail_fit,
        record::record_open_trim,
        record::record_trim_current,
        record::record_info,
        record::record_export,
        record::record_export_cancel,
        record::record_trash,
        // cut-by-text
        transcript::transcript_model,
        transcript::transcript_model_download,
        transcript::transcript_model_remove,
        transcript::transcript_get,
        transcript::transcript_start,
        transcript::transcript_cancel,
        transcript::transcript_save_cuts,
        // settings
        settings::settings_get,
        settings::settings_set,
        settings::settings_reset,
        settings::settings_preview_name,
        settings::settings_folder_status,
        settings::settings_pause_shortcuts,
        settings::settings_default_shortcuts,
        settings::settings_revoke_consent,
        // permissions
        permissions::permissions_status,
        permissions::permissions_request,
        permissions::permissions_open_settings,
        // update
        update::update_state,
        update::update_check,
        update::update_install,
        // windows
        windows::window_open,
        windows::window_open_settings,
        windows::window_take_settings_section,
        windows::toast_fit,
        windows::toast_pointer,
        windows::app_quit,
        windows::app_log,
        windows::app_copy_text,
        windows::app_open_url,
        windows::app_can_open_url,
        windows::app_add_to_calendar,
        windows::app_open_system_settings,
        windows::app_restart,
        windows::app_choose_folder,
        windows::app_choose_file,
        windows::popover_hide,
        windows::popover_fit,
        windows::popover_drag_out,
        windows::pin_open,
        windows::pin_info,
        windows::pin_image,
        windows::pin_frame,
        windows::pin_set_frame,
        windows::pin_set_locked,
        windows::pin_set_all_spaces,
        windows::pin_copy,
        windows::pin_save,
        windows::pin_edit,
        windows::pin_close,
        windows::pin_close_all,
    ]
}
