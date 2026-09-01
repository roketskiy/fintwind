//! Windows tray presence for the close-to-background main window.
//!
//! Closing the window hides it instead of exiting the process, so the
//! supervised daemon keeps OpenCode sessions running. The tray icon owns the
//! two actions a hidden window cannot reach itself — restore and quit — and a
//! named mutex turns a second launch into a "show the running instance"
//! signal rather than a competing process with a second daemon.
//!
//! Tray and menu callbacks fire re-entrantly inside GPUI's Win32 message
//! pump, where GPUI state must not be touched. Handlers only forward
//! [`TrayCommand`]s through an async channel; the foreground task in `init`
//! is the single place that acts on them.

use gpui::{AnyWindowHandle, App};

use crate::identity::APP_NAME;

/// Result of the single-launch check at the top of `run`.
#[derive(Clone, Copy, PartialEq)]
pub enum SingleInstance {
    /// This process owns the app; continue startup.
    Acquired,
    /// Another instance is running, possibly hidden in the tray. It was asked
    /// to show its window and this process must exit without starting a
    /// daemon.
    AlreadyRunning,
}

pub fn acquire_single_instance() -> SingleInstance {
    #[cfg(target_os = "windows")]
    {
        windows_single_instance::acquire()
    }
    #[cfg(not(target_os = "windows"))]
    {
        SingleInstance::Acquired
    }
}

/// Install the tray icon and start its command pump. Call once from the
/// platform run callback, after the main window exists.
#[cfg(target_os = "windows")]
pub fn init(cx: &mut App, window: AnyWindowHandle) {
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, TrayIconBuilder, TrayIconEvent};

    let icon = load_icon();
    let open_id = MenuId::new("fintwind.open");
    let quit_id = MenuId::new("fintwind.quit");
    let open_item =
        MenuItem::with_id(open_id.clone(), tr!("tray.open", app = APP_NAME), true, None);
    let quit_item =
        MenuItem::with_id(quit_id.clone(), tr!("tray.quit", app = APP_NAME), true, None);
    let menu = Menu::new();
    menu.append(&open_item).ok();
    menu.append(&PredefinedMenuItem::separator()).ok();
    menu.append(&quit_item).ok();

    let tray = TrayIconBuilder::new()
        .with_tooltip(APP_NAME)
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .expect("could not create the fintwind tray icon");

    let commands = commands();
    let sender = commands.0.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let command = if event.id == open_id {
            TrayCommand::ShowWindow
        } else if event.id == quit_id {
            TrayCommand::Quit
        } else {
            return;
        };
        let _ = sender.try_send(command);
    }));
    let sender = commands.0.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // Windows emits Click on button release; right click opens the menu
        // instead because `with_menu_on_left_click(false)`.
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            ..
        } = event
        {
            let _ = sender.try_send(TrayCommand::ShowWindow);
        }
    }));

    TRAY.with_borrow_mut(|slot| {
        *slot = Some(TrayState {
            _tray: tray,
            open_item,
            quit_item,
        });
    });

    let receiver = commands.1.clone();
    cx.spawn(async move |cx: &mut gpui::AsyncApp| {
        while let Ok(command) = receiver.recv().await {
            match command {
                TrayCommand::ShowWindow => {
                    window
                        .update(cx, |_, window, _| {
                            crate::platform::show_window(window);
                            window.activate_window();
                        })
                        .ok();
                }
                TrayCommand::Quit => {
                    // Delete the shell icon before the loop stops pumping.
                    TRAY.with_borrow_mut(|slot| *slot = None);
                    cx.update(|cx| cx.quit());
                }
            }
        }
    })
    .detach();
}

#[cfg(not(target_os = "windows"))]
pub fn init(_: &mut App, _: AnyWindowHandle) {}

/// Re-translate the tray menu after a language change.
pub fn refresh_labels() {
    #[cfg(target_os = "windows")]
    TRAY.with_borrow(|slot| {
        let Some(state) = slot.as_ref() else {
            return;
        };
        state.open_item.set_text(tr!("tray.open", app = APP_NAME));
        state.quit_item.set_text(tr!("tray.quit", app = APP_NAME));
    });
}

#[derive(Clone, Copy)]
enum TrayCommand {
    ShowWindow,
    Quit,
}

/// Commands land here from tray callbacks and the single-instance watcher;
/// the foreground task in `init` drains it on GPUI's executor.
#[cfg(target_os = "windows")]
fn commands() -> &'static (
    smol::channel::Sender<TrayCommand>,
    smol::channel::Receiver<TrayCommand>,
) {
    static COMMANDS: std::sync::OnceLock<(
        smol::channel::Sender<TrayCommand>,
        smol::channel::Receiver<TrayCommand>,
    )> = std::sync::OnceLock::new();
    COMMANDS.get_or_init(smol::channel::unbounded)
}

/// Decoded once per process; the shell scales the 256px icon down itself.
#[cfg(target_os = "windows")]
fn load_icon() -> tray_icon::Icon {
    let image = image::load_from_memory(include_bytes!("../website/public/app-icon.png"))
        .expect("embedded app icon decodes");
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    tray_icon::Icon::from_rgba(rgba.into_raw(), width, height)
        .expect("embedded app icon converts to a tray icon")
}

/// Only kept alive: dropping the TrayIcon removes the icon from the shell.
/// muda and tray-icon hold `Rc` internally, so this is main-thread-only
/// state — which every accessor is: `init`, `refresh_labels`, and the
/// foreground command task all run on GPUI's main thread.
#[cfg(target_os = "windows")]
struct TrayState {
    _tray: tray_icon::TrayIcon,
    open_item: tray_icon::menu::MenuItem,
    quit_item: tray_icon::menu::MenuItem,
}

#[cfg(target_os = "windows")]
thread_local! {
    static TRAY: std::cell::RefCell<Option<TrayState>> = const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "windows")]
mod windows_single_instance {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, WAIT_OBJECT_0, ERROR_ALREADY_EXISTS,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject, INFINITE,
    };

    use super::commands;
    use super::SingleInstance;
    use super::TrayCommand;
    use crate::identity::APP_ID;

    /// The show-request event is created *before* the mutex: a second
    /// instance that finds the mutex held can then always find this event to
    /// signal. Both handles are intentionally held for the process lifetime.
    pub(super) fn acquire() -> SingleInstance {
        unsafe {
            let show_event_name = wide(format!("Local\\{APP_ID}.show-window"));
            let show_event = CreateEventW(
                std::ptr::null(),
                0, // auto-reset: each launch request shows the window once
                0,
                show_event_name.as_ptr(),
            );
            let mutex_name = wide(format!("Local\\{APP_ID}.single-instance"));
            let mutex = CreateMutexW(std::ptr::null(), 1, mutex_name.as_ptr());
            if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
                if !show_event.is_null() {
                    SetEvent(show_event);
                    CloseHandle(show_event);
                    CloseHandle(mutex);
                }
                return SingleInstance::AlreadyRunning;
            }
            if !show_event.is_null() {
                let show_event = SendHandle(show_event);
                let _ = std::thread::Builder::new()
                    .name("fintwind-single-instance".into())
                    .spawn(move || forward_show_requests(show_event));
            }
            SingleInstance::Acquired
        }
    }

    /// Raw kernel handles are valid from any thread; the pointer type just
    /// does not carry that fact.
    struct SendHandle(windows_sys::Win32::Foundation::HANDLE);
    unsafe impl Send for SendHandle {}

    fn forward_show_requests(show_event: SendHandle) {
        let show_event = show_event.0;
        loop {
            if unsafe { WaitForSingleObject(show_event, INFINITE) } != WAIT_OBJECT_0 {
                return;
            }
            if commands().0.try_send(TrayCommand::ShowWindow).is_err() {
                return;
            }
        }
    }

    fn wide(value: String) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}
