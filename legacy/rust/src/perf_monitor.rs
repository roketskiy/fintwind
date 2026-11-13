//! The help menu's "Performance Overhead" window: a read-only process table
//! for everything this Fintwind instance owns.
//!
//! Sampling lives in [`crate::perf_sampler`]; this file is the window around
//! it. The window samples while it is open and stops when it closes: the
//! poll loop lives on the view's own task, writes its result into the view,
//! and a failed weak update (the view is gone) ends the loop. Frames only
//! read the cached sample; all Win32 queries run on the background executor.
//! The window is strictly read-only — no kill, restart, or affinity
//! controls.
//!
//! The table is the kit's own [`DataTable`] (`gpui-kit` → `gpui-component`
//! 0.7.1): one component owns the columns, the header, the virtualized rows,
//! the scrolling, the selection modes, and the keyboard behavior — that
//! semantics is the library's, not this viewer's. A [`SampleDelegate`] feeds
//! it the latest sample and renders cells with Ely's `tabular` figures,
//! `Ellipsis` truncation, and number formatting; columns are neither
//! sortable, resizable, nor movable — this is a viewer. Sampling only
//! re-anchors a *row* selection to the same process (verified PID +
//! creation time) after a re-sort, and never touches the component's column
//! or cell selections. The selected process's full name reads in its own
//! strip below the table, and totals sit under that. A refresh never moves
//! the viewport on its own; the component reveals on its own keyboard
//! selections.
//!
//! The window caption: GPUI's normal Windows window ships without
//! `WS_CAPTION` (the platform backend only adds it for `WindowKind::Dialog`,
//! which would also disable the parent), so [`enable_native_caption`] sets
//! that style bit on this one window right after creation. With
//! `appears_transparent: false` the backend then passes frame accounting and
//! hit-testing through to the platform — a real title bar with drag, snap,
//! and min/max/close, without touching the vendor or any other window. A
//! failed call closes the window instead of delivering one without a title
//! bar.
//!
//! The window's lifetime is tied to the main window: the app view closes it
//! on release (the app's release hook), so closing the app cannot strand
//! this sampler beside a dead workspace.

use std::sync::Arc;
use std::time::Duration;

use ely_gpui_component::typography::{Ellipsis, format as ely_format, tabular};
use gpui::{
    AnyWindowHandle, App, Bounds, Context, Entity, FocusHandle, Focusable as _, FontWeight,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Subscription, Task,
    TextAlign, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, point,
    prelude::*, px, rems, size,
};
use gpui_kit::base::actions as kit_cancel;
use gpui_kit::component::{
    Sizable as _, Size,
    table::{Column, DataTable, TableDelegate, TableSelection, TableState},
};

use crate::perf_sampler::Sample;
use crate::theme::Theme;

actions!(fintwind_perf, [Dismiss]);

/// Key context the monitor window declares.
const CONTEXT: &str = "PerfMonitor";

/// How often the open window takes a sample.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

const WINDOW_WIDTH: f32 = 960.0;
const WINDOW_HEIGHT: f32 = 520.0;

/// Register the window's keys. Called once at startup. The table's own keys
/// belong to the kit's `DataTable`; this window only adds `escape` to close.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some(CONTEXT))]);
}

/// Open the monitor window, or bring an existing one to the front. The
/// caller (the app) keeps the handle so the menu item re-activates instead
/// of stacking windows.
pub fn open_window(cx: &mut App) -> anyhow::Result<AnyWindowHandle> {
    let (handle, _) = gpui_kit::open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(tr!("perf.window_title").into()),
                // Keep the platform's frame accounting: with this off the
                // backend's WM_NCCALCSIZE and hit-test handlers pass through
                // to DefWindowProc, so once the caption style bit is added
                // below the title bar behaves natively.
                appears_transparent: false,
                traffic_light_position: None,
            }),
            is_movable: true,
            is_resizable: true,
            app_owns_titlebar_drag: false,
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            app_id: Some(crate::identity::APP_ID.to_owned()),
            // Wide enough that all seven columns fit without scrolling from
            // the first frame — this tool window's initial width only; the
            // user is free to resize it and nothing is persisted behind
            // their back.
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(96.0), px(96.0)),
                size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            ))),
            window_min_size: Some(size(px(560.0), px(320.0))),
            ..Default::default()
        },
        cx,
        |window, cx| {
            let view = cx.new(|cx| PerfMonitor::new(window, cx));
            let focus = view.read(cx).table_focus(cx);
            window.focus(&focus, cx);
            view
        },
    )?;
    // The caption comes after the window exists; a failure closes this
    // window again rather than deliver one without a title bar. The dark
    // mode follows the Fintwind theme (not only the system), so re-apply
    // it after the style change — the vendor sets it once at creation
    // from the system appearance, before this caption exists.
    let caption_dark = crate::theme::Theme::current(cx).is_dark;
    match handle.update(cx, |_, window, _| {
        enable_native_caption(window)?;
        sync_native_caption_theme(window, caption_dark);
        Ok::<(), anyhow::Error>(())
    }) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            anyhow::bail!("the performance window has no title bar: {error}");
        }
        Err(error) => anyhow::bail!("the performance window closed early: {error}"),
    }
    handle.update(cx, |_, window, cx| {
        window.activate_window();
        cx.activate(true);
    })?;
    Ok(handle)
}

/// Add `WS_CAPTION` to this one window. GPUI's normal Windows window is
/// created with `WS_SYSMENU | WS_THICKFRAME | WS_MAXIMIZEBOX |
/// WS_MINIMIZEBOX` and no caption — only `WindowKind::Dialog` gets one, and
/// that kind would also disable its parent, which this parallel viewer must
/// not do. Setting the style bit on the already-created HWND and asking for
/// one frame recalculation gives the window the platform title bar with its
/// drag region and min/max/close buttons. A failed call is an error: the
/// caller closes the window rather than deliver one without a title bar.
#[cfg(windows)]
fn enable_native_caption(window: &Window) -> anyhow::Result<()> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::{GetLastError, HWND, SetLastError};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_CAPTION,
    };

    // The inherent `Window::window_handle` returns the app's window registry
    // entry; the raw handle comes from the `HasWindowHandle` trait, whose
    // borrowed `WindowHandle` hands out the platform-specific value.
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        anyhow::bail!("the platform did not hand out the window handle");
    };
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        anyhow::bail!("the platform window is not a Win32 window");
    };
    let hwnd = win.hwnd.get() as isize as HWND;
    unsafe {
        // Both style calls report failure as 0 with a pending error code;
        // clearing it first keeps a legitimate 0 style readable.
        SetLastError(0);
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        if style == 0 && GetLastError() != 0 {
            anyhow::bail!("could not read the window style (error {})", GetLastError());
        }
        if style & WS_CAPTION as isize != 0 {
            return Ok(());
        }
        if SetWindowLongPtrW(hwnd, GWL_STYLE, style | WS_CAPTION as isize) == 0 {
            anyhow::bail!("could not set the caption style (error {})", GetLastError());
        }
        if SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        ) == 0
        {
            anyhow::bail!(
                "could not recalculate the window frame (error {})",
                GetLastError()
            );
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn enable_native_caption(_: &Window) -> anyhow::Result<()> {
    Ok(())
}

/// Match the native caption to the Fintwind theme. The vendor paints the
/// caption from the *system* appearance once at creation — before this
/// window even has a caption — so without this a Fintwind-dark window on
/// a light system (or a caption added late) keeps a white title bar.
/// Re-applying after the style change and on later theme switches keeps
/// the two in step. Failures stay silent: the worst case is the previous
/// title bar, never a closed window.
#[cfg(windows)]
fn sync_native_caption_theme(window: &Window, is_dark: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        return;
    };
    let hwnd = win.hwnd.get() as isize as *mut _;
    let dark: i32 = if is_dark { 1 } else { 0 };
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &dark as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn sync_native_caption_theme(_: &Window, _: bool) {}

/// The stable identity of a sampled process: the PID together with the
/// creation time the ownership walk verified. Selection travels with the
/// identity, so a re-sorted sample cannot move it under the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    create_time: u64,
}

impl ProcessIdentity {
    fn of(row: &crate::perf_sampler::SampleRow) -> Self {
        Self {
            pid: row.pid,
            create_time: row.create_time,
        }
    }
}

/// What one refresh should do with a row selection: leave it where it is,
/// move it with its process, or let it go because the process left the
/// tree. Column, cell, and empty selections never reach this.
enum Reanchor {
    SameRow,
    Move(usize),
    Gone,
    Keep,
}

/// The delegate behind the kit's `DataTable`: it owns the latest sample and
/// renders every cell from it. Data lands once per sample; the component
/// virtualizes the rows, so cells render only in view.
struct SampleDelegate {
    sample: Option<Arc<Sample>>,
    /// Built once at construction; the delegate's `column` hook reads this
    /// instead of rebuilding the set on every prepare.
    columns: Vec<Column>,
}

impl SampleDelegate {
    /// The seven columns, shared by header and cells through the delegate's
    /// `column` hook. Numbers read from the end; none of them sort, resize,
    /// or move — this is a viewer.
    fn columns() -> Vec<Column> {
        vec![
            Column::new("name", tr!("perf.col_process")).width(px(260.0)),
            numeric("pid", "PID", px(72.0)),
            numeric("cpu", tr!("perf.col_cpu"), px(88.0)),
            numeric("private", tr!("perf.col_private"), px(110.0)),
            numeric("working_set", tr!("perf.col_working_set"), px(110.0)),
            numeric("io_read", tr!("perf.col_io_read"), px(118.0)),
            numeric("io_write", tr!("perf.col_io_write"), px(118.0)),
        ]
    }

    /// One cell's text; an em dash stands in for unknown so a gap never
    /// reads as zero.
    fn cell_text(row: &crate::perf_sampler::SampleRow, col_ix: usize) -> SharedString {
        match col_ix {
            0 => row.name.clone().into(),
            1 => row.pid.to_string().into(),
            2 => number(row.cpu_percent.map(f64::from), "%"),
            3 => number(row.private_mb, "MB"),
            4 => number(row.working_set_mb, "MB"),
            5 => number(row.io_read_mb, "MB"),
            6 => number(row.io_write_mb, "MB"),
            _ => SharedString::default(),
        }
    }
}

fn numeric(key: &'static str, name: impl Into<SharedString>, width: gpui::Pixels) -> Column {
    Column {
        align: TextAlign::Right,
        resizable: false,
        movable: false,
        ..Column::new(key, name).width(width)
    }
}

/// A number as the table reads it: Ely's formatting with one decimal.
fn number(value: Option<f64>, unit: &str) -> SharedString {
    match value {
        Some(value) => format!(
            "{} {unit}",
            ely_format::number(value, 1, ely_format::Separators::EN)
        )
        .into(),
        None => "—".into(),
    }
}

impl TableDelegate for SampleDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.sample
            .as_ref()
            .map(|sample| sample.rows.len())
            .unwrap_or(0)
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns
            .get(col_ix)
            .cloned()
            .unwrap_or_else(|| Column::new("", ""))
    }

    /// Number column headers read from the end, like their cells do. The
    /// kit's cell chrome already sizes and pads head and body cells alike;
    /// this only places the text.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let cell = div().size_full().flex().items_center();
        if column.align == TextAlign::Right {
            cell.justify_end()
        } else {
            cell
        }
        .child(column.name)
    }

    /// One cell from the sample. Ely's `Ellipsis` keeps overlong names to
    /// the row (the strip below the table reads the selected one in full)
    /// and `tabular` keeps the figures column-aligned. No extra padding:
    /// the kit's cell chrome handles head and body cells identically.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let text = self
            .sample
            .as_ref()
            .and_then(|sample| sample.rows.get(row_ix))
            .map(|row| (SampleDelegate::cell_text(row, col_ix), col_ix == 0));
        let mut cell = div().flex().items_center().w_full();
        match text {
            Some((text, is_name)) => {
                if is_name {
                    cell = cell.child(Ellipsis::new(text));
                } else {
                    cell = cell.justify_end().child(tabular(div()).child(text));
                }
            }
            None => cell = cell.child(SharedString::default()),
        }
        cell
    }

    /// The empty state stays bare: the first sample lands within a moment,
    /// and an inbox glyph adds nothing.
    fn render_empty(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
    }
}

/// The monitor window's root view. Holds the kit table state (which owns
/// the sample and the selection) and the poll task; dropping the view drops
/// the task, which is what stops sampling.
struct PerfMonitor {
    table_state: Entity<TableState<SampleDelegate>>,
    /// Keeps the table's notify → view redraw path alive: a selection made
    /// inside the kit updates the full-name strip immediately instead of
    /// waiting for the next sample. No feedback loop — the table's own
    /// render only writes its options back and never notifies.
    _selection_observer: Subscription,
    failed: Option<SharedString>,
    /// Last dark mode pushed to the native caption. The content follows
    /// `Theme::current` through render; the Win32 caption does not, so
    /// render re-applies it only on change.
    caption_dark: Option<bool>,
    _task: Task<()>,
}

impl PerfMonitor {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table_state = cx.new(|cx| {
            TableState::new(
                SampleDelegate {
                    sample: None,
                    columns: SampleDelegate::columns(),
                },
                window,
                cx,
            )
            .col_movable(false)
            .col_resizable(false)
            .sortable(false)
        });
        let selection_observer = cx.observe(&table_state, |_, _, cx| {
            cx.notify();
        });
        let task = cx.spawn(async move |this, cx| {
            let mut sampler = crate::perf_sampler::Sampler::default();
            loop {
                // The sampler travels through the background spawn and back,
                // so its CPU baselines outlive one sample without any lock
                // the UI thread could touch.
                let (sample, sampler_back) = cx
                    .background_executor()
                    .spawn(async move {
                        let sample = sampler.sample();
                        (sample, sampler)
                    })
                    .await;
                sampler = sampler_back;
                let alive = this
                    .update(cx, |view, cx| {
                        match sample {
                            Ok(sample) => {
                                let sample = Arc::new(sample);
                                view.table_state.update(cx, |state, cx| {
                                    // Only a *row* selection gets re-anchored
                                    // to the same process after a re-sort. The
                                    // kit's column and cell selections are the
                                    // component's own semantics; sampling
                                    // leaves them exactly as the user left
                                    // them.
                                    let reanchor = match state.selection() {
                                        TableSelection::Row(row_ix) => {
                                            let identity = state
                                                .delegate()
                                                .sample
                                                .as_ref()
                                                .and_then(|old| old.rows.get(row_ix))
                                                .map(ProcessIdentity::of);
                                            let next_ix = identity
                                                .and_then(|identity| {
                                                    sample.rows.iter().position(|row| {
                                                        ProcessIdentity::of(row) == identity
                                                    })
                                                })
                                                .map(|ix| (row_ix, ix));
                                            match next_ix {
                                                Some((row_ix, ix)) if ix == row_ix => {
                                                    Reanchor::SameRow
                                                }
                                                Some((_, ix)) => Reanchor::Move(ix),
                                                None => Reanchor::Gone,
                                            }
                                        }
                                        // Column and cell selections, and no
                                        // selection, pass untouched.
                                        _ => Reanchor::Keep,
                                    };
                                    state.delegate_mut().sample = Some(sample);
                                    match reanchor {
                                        // A same-row refresh skips the setter:
                                        // no event, and no scroll decision.
                                        Reanchor::SameRow => {}
                                        // The process moved within the tree:
                                        // re-anchor, preserving whatever
                                        // scroll the user's own keys already
                                        // asked for and dropping the one this
                                        // refresh created.
                                        Reanchor::Move(ix) => {
                                            let pending = state
                                                .vertical_scroll_handle
                                                .0
                                                .borrow()
                                                .deferred_scroll_to_item;
                                            state.set_selected_row(ix, cx);
                                            state
                                                .vertical_scroll_handle
                                                .0
                                                .borrow_mut()
                                                .deferred_scroll_to_item = pending;
                                        }
                                        // The selected process left the tree;
                                        // clearing is honest only here, where
                                        // a row selection actually existed.
                                        Reanchor::Gone => state.clear_selection(cx),
                                        // Column/cell selections and no
                                        // selection pass untouched.
                                        Reanchor::Keep => {}
                                    }
                                    cx.notify();
                                });
                                view.failed = None;
                            }
                            // A failed sample keeps the last good table on
                            // screen; the error shows above it until the
                            // next sample succeeds.
                            Err(error) => {
                                view.failed = Some(tr!("perf.sample_failed", error = error).into());
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                // The view is gone: the window closed, so stop sampling.
                if !alive {
                    return;
                }
                cx.background_executor().timer(SAMPLE_INTERVAL).await;
            }
        });
        Self {
            table_state,
            _selection_observer: selection_observer,
            failed: None,
            caption_dark: None,
            _task: task,
        }
    }

    /// The table's focus handle, so the window opens with the rows in
    /// keyboard reach.
    fn table_focus(&self, cx: &App) -> FocusHandle {
        self.table_state.read(cx).focus_handle(cx)
    }

    /// The selected process's full name for the strip under the table.
    fn selected_name(&self, cx: &App) -> Option<String> {
        let state = self.table_state.read(cx);
        let ix = state.selected_row()?;
        let row = state.delegate().sample.as_ref()?.rows.get(ix)?;
        Some(row.name.clone())
    }
}

impl Render for PerfMonitor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        // Keep the native caption with the Fintwind theme. Content re-renders
        // on theme change; the Win32 caption only follows an explicit
        // `DwmSetWindowAttribute`, so push it here on change only.
        if self.caption_dark != Some(theme.is_dark) {
            sync_native_caption_theme(window, theme.is_dark);
            self.caption_dark = Some(theme.is_dark);
        }
        let mut root = div()
            .key_context(CONTEXT)
            // The kit's `DataTable` binds `escape` to its own `Cancel` one
            // context deeper, and actions consume keystrokes before bubble
            // key handlers run. Capturing the `Cancel` action here — the
            // capture phase reaches this root before the table's target —
            // keeps `escape` closing this window wherever the focus sits,
            // without touching the kit's default binding. With no table
            // focused, `escape` still falls to `Dismiss` below.
            .capture_action(move |_: &kit_cancel::Cancel, window, cx: &mut App| {
                window.remove_window();
                cx.stop_propagation();
            })
            .on_action(cx.listener(|_: &mut Self, _: &Dismiss, window, _| {
                window.remove_window();
            }))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.canvas)
            .font_family(crate::theme::ui_font_family())
            .text_color(theme.text)
            .text_size(crate::theme::ui_px(12.0))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .px(rems(0.5))
                    .pt(rems(0.5))
                    .child(
                        DataTable::new(&self.table_state)
                            .with_size(Size::Small)
                            .scrollbar_visible(true, true),
                    ),
            );

        // A failed round shows as a strip; the last good table stays put.
        if let Some(error) = self.failed.clone() {
            root = root.child(
                div()
                    .flex_none()
                    .px(rems(1.0))
                    .py(rems(0.5))
                    .border_t_1()
                    .border_color(theme.border)
                    .text_color(theme.text_secondary)
                    .child(error),
            );
        }
        // The selected process's full name, in its own strip: it wraps there
        // instead of fighting the row height. Nothing shows when nothing is
        // selected.
        if let Some(name) = self.selected_name(cx) {
            root = root.child(
                div()
                    .flex_none()
                    .px(rems(1.0))
                    .py(rems(0.5))
                    .border_t_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(name),
            );
        }
        root = root.child(render_totals(&self.table_state, cx));
        root
    }
}

/// The totals strip under the table. Working set is deliberately absent:
/// its sum would double-count pages shared between the listed processes. A
/// total is shown only when every row's value for that measure is known —
/// a partial sum would read as complete.
fn render_totals(
    table_state: &Entity<TableState<SampleDelegate>>,
    cx: &mut Context<PerfMonitor>,
) -> impl IntoElement {
    let theme = Theme::current(cx);
    let sample = table_state.read(cx).delegate().sample.clone();
    let read = |value: Option<f64>, unit: &str| -> String {
        match value {
            Some(value) => format!(
                "{} {unit}",
                ely_format::number(value, 1, ely_format::Separators::EN)
            ),
            None => "—".to_owned(),
        }
    };
    let (cpu, private, io_read, io_write) = match sample.as_deref() {
        Some(sample) => (
            sample.total_cpu_percent,
            sample.total_private_mb,
            sample.total_io_read_mb,
            sample.total_io_write_mb,
        ),
        None => (None, None, None, None),
    };
    let cpu = match cpu {
        Some(value) => format!(
            "{}%",
            ely_format::number(f64::from(value), 1, ely_format::Separators::EN)
        ),
        None => "—".to_owned(),
    };
    let group = |label: SharedString, value: String| {
        div()
            .flex()
            .items_baseline()
            .gap(rems(0.25))
            .child(div().text_color(theme.text_tertiary).child(label))
            .child(tabular(div()).text_color(theme.text_secondary).child(value))
    };
    div()
        .id("perf-totals")
        .flex_none()
        .flex()
        .items_center()
        .flex_wrap()
        .gap(rems(1.25))
        .px(rems(1.0))
        .py(rems(0.5))
        .border_t_1()
        .border_color(theme.border)
        .font_family(crate::theme::ui_font_family())
        .text_size(crate::theme::ui_px(12.0))
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_secondary)
                .child(tr!("perf.total")),
        )
        .child(group(tr!("perf.col_cpu").into(), cpu))
        .child(group(tr!("perf.col_private").into(), read(private, "MB")))
        .child(group(tr!("perf.col_io_read").into(), read(io_read, "MB")))
        .child(group(tr!("perf.col_io_write").into(), read(io_write, "MB")))
}
