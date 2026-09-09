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

pub fn configure_sidebar_material(_: &Window, _: bool) {}

pub fn set_sidebar_material_width(_: &Window, _: f32) {}

pub fn set_window_appearance(_: &Window, _: Option<bool>) {}
