package main

import (
	"github.com/egoist/mygo/ui"
)

// Panel width bounds from Fintwind's app.rs. The file-tree bounds already
// live in rightpanel.go; the sidebar and right-panel bounds live here.
const (
	sidebarMinWidth    = 180.0
	sidebarMaxWidth    = 420.0
	rightPanelMinWidth = 280.0
	rightPanelMaxWidth = 1000.0
	// resizeHandleHitWidth is the transparent grab strip on a panel edge.
	// Rust uses 8-10 px hit strips (render_panel_resize_handle); the strip
	// stays inside the panel because the sidebar clips its children and
	// there is no webview compositing above mygo content.
	resizeHandleHitWidth = 8.0
)

// sidebarResizeHandle is the Sidebar target's edge: the sidebar's right
// border. Dragging right widens it, like resize_panel_mouse_move's
// start_width + delta.
func (a *app) sidebarResizeHandle(c *ui.Context) {
	handle := ui.Row(c).Absolute().Top(0).Right(0).Width(resizeHandleHitWidth).
		FillHeight().Cursor(ui.CursorResizeEW)
	if dx, _, ok := handle.Dragged(); ok && dx != 0 {
		a.sidebarWidth = min(max(a.sidebarWidth+dx, sidebarMinWidth), sidebarMaxWidth)
	}
}

// rightPanelResizeHandle is the RightPanel target's edge: the panel's left
// border. Dragging left widens it, like start_width - delta. It renders on
// every surface, as the Rust handle is a child of the panel itself.
func (a *app) rightPanelResizeHandle(c *ui.Context) {
	handle := ui.Row(c).Absolute().Top(0).Left(0).Width(resizeHandleHitWidth).
		FillHeight().Cursor(ui.CursorResizeEW)
	if dx, _, ok := handle.Dragged(); ok && dx != 0 {
		a.rightPanelWidthValue = min(max(a.rightPanelWidthValue-dx, rightPanelMinWidth), rightPanelMaxWidth)
	}
}

// fileTreeResizeHandle is the FileTree target's edge: the tree column's
// left border in the file editor split. Dragging left widens it, and the
// editor never drops below fileEditorMinWidth, like sessions.rs.
func (a *app) fileTreeResizeHandle(c *ui.Context) {
	handle := ui.Row(c).Absolute().Top(0).Left(0).Width(resizeHandleHitWidth).
		FillHeight().Cursor(ui.CursorResizeEW)
	if dx, _, ok := handle.Dragged(); ok && dx != 0 {
		maximum := min(fileTreeMaxWidth, a.rightPanelWidthValue-fileEditorMinWidth)
		maximum = max(maximum, fileTreeMinWidth)
		a.fileTreeWidth = min(max(a.fileTreeWidth-dx, fileTreeMinWidth), maximum)
	}
}
