package main

import (
	"sync/atomic"

	"github.com/google/uuid"
)

// Native panel state lives here, never in main.go beyond the single field
// the main agent owns. One entry per session; every browser and terminal tab
// carries its own unique tabID. Surfaces and the active tab are stored per
// session here because sessionstate.go does not persist them: switching
// sessions saves the outgoing pair and restores the incoming pair, so each
// session keeps its own right-panel tabs. Tab keys are fixed before building
// so a later render (CrychicUI or otherwise) can call c.Key with them.
//
// Nothing here stores ui.Element or ui.Context. Background work posts results
// through a.post and rechecks generations before touching state.
type nativePanelState struct {
	sessions map[string]*nativeSessionPanels
	ports    nativePortsState
}

// nativeSessionPanels is everything the right panel owns for one session.
type nativeSessionPanels struct {
	sessionID     string
	browsers      []*nativeBrowserTab
	terminals     []*nativeTerminalTab
	files         map[string]*nativeFileEditor
	surfaces      []string
	activeSurface int
}

// nativeTabKey fixes the identity a tab build must use with c.Key before
// constructing any stateful control.
func nativeTabKey(tabID string) string {
	return "native:" + tabID
}

// nativePanelsFor returns the session entry, creating it on first use. Main
// thread only.
func (a *app) nativePanelsFor(sessionID string) *nativeSessionPanels {
	if a.native.sessions == nil {
		a.native.sessions = map[string]*nativeSessionPanels{}
	}
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		entry = &nativeSessionPanels{
			sessionID: sessionID,
			files:     map[string]*nativeFileEditor{},
		}
		a.native.sessions[sessionID] = entry
	}
	if entry.files == nil {
		entry.files = map[string]*nativeFileEditor{}
	}
	return entry
}

// switchNativeSession saves the outgoing session's tabs and restores the
// incoming session's tabs. Entries of other sessions stay alive so switching
// back restores their terminals, views and file drafts. Main thread only.
func (a *app) switchNativeSession(oldID, newID string) {
	if oldID != "" {
		entry := a.nativePanelsFor(oldID)
		entry.surfaces = append([]string(nil), a.surfaces...)
		entry.activeSurface = a.activeSurface
	}
	if newID == "" {
		return
	}
	entry := a.nativePanelsFor(newID)
	a.surfaces = append([]string(nil), entry.surfaces...)
	a.activeSurface = entry.activeSurface
	if len(a.surfaces) == 0 {
		a.activeSurface = -1
	} else if a.activeSurface < 0 || a.activeSurface >= len(a.surfaces) {
		a.activeSurface = len(a.surfaces) - 1
	}
	a.syncSelectedFile()
}

// closeNativePanels releases everything this file created: owned terminals
// are closed, owned web views destroyed, background launches cancelled by
// generation. Main thread only, called from OnClosed.
func (a *app) closeNativePanels() {
	for sessionID, entry := range a.native.sessions {
		if entry == nil {
			continue
		}
		for _, tab := range entry.terminals {
			a.disposeNativeTerminal(tab)
		}
		for _, tab := range entry.browsers {
			a.disposeNativeBrowser(tab)
		}
		delete(a.native.sessions, sessionID)
	}
}

// postOrReleaseOrDrop runs update on the main thread unless the app already
// closed: a.post silently drops closures after OnClosed, which would leak a
// resource the background thread just built. Callers pass a release that
// frees exactly the resource they built (nil when there is nothing to free).
// Exactly one of update and release runs, decided by an atomic claim: the
// update wrapper claims before adopting, and a waiter claims on done for the
// release. Background goroutines only; the waiter never blocks the UI thread.
func (a *app) postOrReleaseOrDrop(release func(), update func()) {
	if release == nil {
		a.post(update)
		return
	}
	var claimed atomic.Bool
	adopted := make(chan struct{})
	a.post(func() {
		if claimed.CompareAndSwap(false, true) {
			update()
			close(adopted)
		}
	})
	go func() {
		select {
		case <-adopted:
			return
		case <-a.done:
		}
		if claimed.CompareAndSwap(false, true) {
			release()
		}
	}()
}

// removeNativeBrowserTab drops a tab from its session list. Callers dispose
// the resource first.
func (entry *nativeSessionPanels) removeNativeBrowserTab(tab *nativeBrowserTab) {
	for i, other := range entry.browsers {
		if other == tab {
			entry.browsers = append(entry.browsers[:i], entry.browsers[i+1:]...)
			return
		}
	}
}

// removeNativeTerminalTab drops a tab from its session list. Callers dispose
// the resource first.
func (entry *nativeSessionPanels) removeNativeTerminalTab(tab *nativeTerminalTab) {
	for i, other := range entry.terminals {
		if other == tab {
			entry.terminals = append(entry.terminals[:i], entry.terminals[i+1:]...)
			return
		}
	}
}

// newNativeTabID mints the unique identity of one browser or terminal tab.
func newNativeTabID() string {
	return uuid.NewString()
}

// Native tabs address their surfaces as "browser:<tabID>" and
// "terminal:<tabID>": one unique surface per tab, mirroring how file tabs
// use "file:<relative>". Single-instance surfaces keep their bare kinds
// ("files", "ports").
const (
	browserTabPrefix  = "browser:"
	terminalTabPrefix = "terminal:"
)

// browserTabID splits a "browser:<tabID>" surface kind into its tab ID.
func browserTabID(kind string) (string, bool) {
	return splitTabKind(kind, browserTabPrefix)
}

// terminalTabID splits a "terminal:<tabID>" surface kind into its tab ID.
func terminalTabID(kind string) (string, bool) {
	return splitTabKind(kind, terminalTabPrefix)
}

func splitTabKind(kind, prefix string) (string, bool) {
	if len(kind) <= len(prefix) || kind[:len(prefix)] != prefix {
		return "", false
	}
	id := kind[len(prefix):]
	if id == "" {
		return "", false
	}
	return id, true
}

// lookupNativeBrowserTab finds a live browser tab by ID without a generation,
// for render and surface actions. Late callbacks keep using the
// generation-checked findNativeBrowserTab.
func (a *app) lookupNativeBrowserTab(sessionID, tabID string) *nativeBrowserTab {
	entry, ok := a.native.sessions[sessionID]
	if !ok || entry == nil {
		return nil
	}
	for _, tab := range entry.browsers {
		if tab != nil && tab.tabID == tabID && !tab.closed {
			return tab
		}
	}
	return nil
}

// lookupNativeTerminalTab finds a live terminal tab by ID without a
// generation, for render and surface actions.
func (a *app) lookupNativeTerminalTab(sessionID, tabID string) *nativeTerminalTab {
	entry, ok := a.native.sessions[sessionID]
	if !ok || entry == nil {
		return nil
	}
	for _, tab := range entry.terminals {
		if tab != nil && tab.tabID == tabID && !tab.closed {
			return tab
		}
	}
	return nil
}
