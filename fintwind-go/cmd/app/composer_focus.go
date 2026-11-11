package main

// Composer auto-focus, mirroring the Rust composer_focus switch points.
//
// Rust focuses the composer exactly at switch points (sessions.rs:
// new_session_action, return_to_sessions, focus_composer_action;
// sidebar.rs: start_session_for_project), never every frame and never
// when the user clicks elsewhere on the session page.
//
// mygo semantics (ui/input.go in the mygo module):
//
//	Focus()     steals the keyboard in every frame it runs — one-shot only.
//	AutoFocus() fires only while st.born == the current frame, i.e. once
//	per element identity. The composer TextArea keeps one identity across
//	sessions (same call site, no Key), so AutoFocus would fire only on
//	first mount and miss every later switch — it cannot express "focus
//	again on session change".
//
// So the render detects the session change itself: the first composer
// frame for a new session key issues one Focus() and records the key.
// Later frames with the same key do nothing, so clicks elsewhere keep
// their focus. Single window: package-level state is acceptable.

// composerFocusedSession is the last session key that already received
// its one-shot composer focus. "\x00none" forces focus on first mount.
var composerFocusedSession = "\x00none"

// composerFocusKey identifies the input box on screen: one per session,
// plus one shared "new" key for the New-task draft (selectedID == "").
func composerFocusKey(a *app) string {
	if a.selectedID == "" {
		return "new"
	}
	return "s:" + a.selectedID
}

// composerShouldFocus reports whether this frame must take the keyboard
// once: only the first composer frame after the session key changes
// (open/switch session, New task, first session page after startup).
// It records the key, so every later frame with the same session leaves
// the focus alone.
func composerShouldFocus(a *app) bool {
	key := composerFocusKey(a)
	if key == composerFocusedSession {
		return false
	}
	composerFocusedSession = key
	return true
}
