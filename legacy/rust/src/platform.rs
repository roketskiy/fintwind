use gpui::Window;

pub fn show_about_panel() {}

/// Register embedded font data with the platform font stack. GPUI's
/// `add_fonts` feeds its private font-kit source on Windows, so this is a
/// no-op here.
pub fn register_fonts_with_coretext(_: &[&'static [u8]]) -> anyhow::Result<()> {
    Ok(())
}

/// Ease of Access → "Show animations in Windows" clears
/// `SPI_GETCLIENTAREAANIMATION`. GPUI has no Windows implementation of its
/// own, and the call only reads a cached user setting, so startup can ask
/// directly.
pub fn init_reduce_motion(cx: &mut gpui::App) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };

    let mut animations_enabled: i32 = 1;
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            std::ptr::from_mut(&mut animations_enabled).cast(),
            0,
        )
    };
    if read != 0 {
        cx.set_reduce_motion(animations_enabled == 0);
    }
}

pub fn show_task_notification(tag: &str, title: &str, body: &str, cx: &gpui::App) {
    cx.show_system_notification(gpui::SystemNotification {
        tag: tag.to_owned().into(),
        title: title.to_owned().into(),
        body: body.to_owned().into(),
        actions: Vec::new(),
    });
}

/// Select `path` in File Explorer. GPUI dispatches this away from the UI
/// thread.
pub fn reveal_in_file_manager(path: &std::path::Path, cx: &gpui::App) {
    cx.reveal_path(path);
}

/// Open `path` with its default application — a document in its editor.
pub fn open_with_default_app(path: &std::path::Path, cx: &gpui::App) {
    cx.open_with_system(path);
}

pub fn configure_main_window_close_behavior(_: &Window, _: &gpui::App) {}

pub fn hide_window(window: &mut Window) {
    window.remove_window();
}

pub fn start_window_move(window: &Window) {
    window.start_window_move();
}

/// Windows performs the user's configured caption double-click action in
/// `DefWindowProc`, which sees the click because the drag region reports
/// itself as caption to the hit test.
pub fn titlebar_double_click(window: &Window) {
    let _ = window;
}

/// Hold the display and system awake while the app runs.
///
/// `SetThreadExecutionState` is thread-scoped: a continuous request outlives
/// the call but ends with the thread that made it. The UI thread lives as
/// long as the process, so it owns the request; a call from another thread is
/// refused instead of clearing a request it does not own.
pub fn set_keep_awake(hold: bool) {
    use parking_lot::Mutex;
    use std::thread::{self, ThreadId};
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };

    static HOLDER: Mutex<Option<(ThreadId, bool)>> = Mutex::new(None);

    let thread = thread::current().id();
    let mut holder = HOLDER.lock();
    match holder.as_mut() {
        Some((owner, current)) => {
            if *owner != thread {
                eprintln!("keep-awake must be requested from its owning thread");
                return;
            }
            if *current == hold {
                return;
            }
            *current = hold;
        }
        None => *holder = Some((thread, hold)),
    }
    let flags = if hold {
        ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    // The return value is the previous thread execution state, which is 0 both
    // when the thread held none and when the call failed. The flags are
    // constants, so success is the expected outcome; treat it as such and
    // record the request rather than inferring a failure from that value.
    let _ = unsafe { SetThreadExecutionState(flags) };
}

pub fn configure_sidebar_material(_: &Window, _: bool) {}

pub fn set_sidebar_material_width(_: &Window, _: f32) {}

pub fn set_window_appearance(_: &Window, _: Option<bool>) {}
