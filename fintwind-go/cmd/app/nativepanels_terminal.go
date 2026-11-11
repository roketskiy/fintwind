package main

import (
	"errors"

	"fintwind-go/internal/model"

	"github.com/egoist/mygo/plugins/terminal"
	"github.com/egoist/mygo/ui"
)

// A nativeTerminalTab is one live shell behind a right-panel tab. Only
// local sessions run: a MyGo plugin pty created in a background goroutine.
// A tab whose project needs another daemon holds an honest error instead of
// a borrowed local shell: the app boots one local Windows daemon and owns no
// client to a target environment, so no openTerminal is ever sent at a
// foreign id. A tab without a usable project holds an error instead of a
// borrowed local shell.
type nativeTerminalTab struct {
	tabID     string
	sessionID string
	title     string
	cwd       string
	err       string
	term      *terminal.Terminal
	starting  bool
	closed    bool
	exited    bool
	exitCode  int
	gen       uint64
	// findOpen holds kero's find bar over this tab; the query searches the
	// screen and scrollback, newest match first.
	findOpen  bool
	findQuery string
	findFocus bool
	findInput ui.Handle
}

// Stable automation labels for the terminal surface.
const (
	nativeTerminalLabel      = "native-terminal"
	nativeTerminalErrorLabel = "native-terminal-error"
	nativeTerminalFindLabel  = "native-terminal-find"
)

// startNativeTerminal opens a tab and boots its shell off the render path.
// The backend comes from resolveTerminalBackend: local projects start a
// plugin pty, remote environments bridge the daemon. Main thread only.
func (a *app) startNativeTerminal(sessionID string) *nativeTerminalTab {
	entry := a.nativePanelsFor(sessionID)
	tab := &nativeTerminalTab{
		tabID:     newNativeTabID(),
		sessionID: sessionID,
		title:     "Terminal",
		gen:       1,
	}
	entry.terminals = append(entry.terminals, tab)
	cwd, remote, err := a.resolveTerminalBackend(sessionID)
	if err != nil {
		tab.err = err.Error()
		return tab
	}
	tab.cwd = cwd
	if remote {
		tab.err = tr("Remote shell needs its target daemon connection, which is not implemented")
		return tab
	}
	if cwd == "" {
		tab.err = tr("No local shell for this session")
		return tab
	}
	tab.starting = true
	a.bootNativeTerminal(sessionID, tab)
	return tab
}

// bootNativeTerminal creates the tab's shell off the render path with
// kero's terminal options. Main thread only; the pty lands via post.
func (a *app) bootNativeTerminal(sessionID string, tab *nativeTerminalTab) {
	gen := tab.gen
	tabID := tab.tabID
	options := a.terminalOptions(tab.cwd)
	options.OnTitle = func(title string) {
		a.post(func() {
			a.applyNativeTerminalTitle(sessionID, tabID, gen, title)
		})
	}
	options.OnExit = func(code int) {
		a.post(func() {
			a.applyNativeTerminalExit(sessionID, tabID, gen, code)
		})
	}
	go func() {
		term, termErr := terminal.New(options)
		a.postOrReleaseOrDrop(func() {
			if term != nil {
				_ = term.Close()
			}
		}, func() {
			a.finishNativeTerminalStart(sessionID, tabID, gen, term, termErr)
		})
	}()
}

// finishNativeTerminalStart adopts a booted shell or releases it when the tab
// already moved on. Runs on the main thread inside a.post.
func (a *app) finishNativeTerminalStart(sessionID, tabID string, gen uint64, term *terminal.Terminal, err error) {
	tab := a.findNativeTerminalTab(sessionID, tabID, gen)
	if tab == nil {
		if term != nil {
			_ = term.Close()
		}
		return
	}
	tab.starting = false
	if err != nil {
		tab.err = err.Error()
		return
	}
	tab.term = term
	if term != nil {
		term.SetFont(terminalFont())
	}
}

// restartNativeTerminal boots a fresh shell in an exited tab, keeping its
// place: kero's Restarted banner equivalent is the clean prompt.
func (a *app) restartNativeTerminal(sessionID, tabID string) {
	tab := a.lookupNativeTerminalTab(sessionID, tabID)
	if tab == nil || tab.closed || tab.starting {
		return
	}
	if tab.term != nil {
		tab.term.EndSearch()
		_ = tab.term.Close()
		tab.term = nil
	}
	tab.exited, tab.exitCode = false, 0
	tab.err = ""
	tab.findOpen, tab.findQuery, tab.findFocus = false, "", false
	tab.gen++
	tab.starting = true
	a.bootNativeTerminal(sessionID, tab)
}

// applyNativeTerminalTitle records a program-set title. Late arrivals after
// close or a newer generation are dropped. Main thread only.
func (a *app) applyNativeTerminalTitle(sessionID, tabID string, gen uint64, title string) {
	if tab := a.findNativeTerminalTab(sessionID, tabID, gen); tab != nil {
		tab.title = title
	}
}

// applyNativeTerminalExit marks the tab exited. The term stays open for
// scrollback until the tab itself closes. Main thread only.
func (a *app) applyNativeTerminalExit(sessionID, tabID string, gen uint64, code int) {
	if tab := a.findNativeTerminalTab(sessionID, tabID, gen); tab != nil {
		tab.exited = true
		tab.exitCode = code
	}
}

// findNativeTerminalTab locates a live tab by identity and generation.
// Anything closed, removed, or superseded returns nil so late callbacks
// release instead of applying.
func (a *app) findNativeTerminalTab(sessionID, tabID string, gen uint64) *nativeTerminalTab {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return nil
	}
	for _, tab := range entry.terminals {
		if tab.tabID == tabID && tab.gen == gen && !tab.closed {
			return tab
		}
	}
	return nil
}

// closeNativeTerminalTab closes the tab the user dismissed: exactly the term
// this file created and still holds is closed, once. Main thread only.
func (a *app) closeNativeTerminalTab(sessionID, tabID string) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	for _, tab := range entry.terminals {
		if tab.tabID != tabID {
			continue
		}
		entry.removeNativeTerminalTab(tab)
		a.disposeNativeTerminal(tab)
		return
	}
}

// disposeNativeTerminal releases one tab's shell. Closing is idempotent:
// only a term this tab still holds is closed, never a foreign handle.
// Main thread only.
func (a *app) disposeNativeTerminal(tab *nativeTerminalTab) {
	if tab == nil || tab.closed {
		return
	}
	tab.closed = true
	if tab.term != nil {
		_ = tab.term.Close()
		tab.term = nil
	}
	tab.starting = false
}

// nilUUID is the empty environment id local daemons require.
const nilUUID = "00000000-0000-0000-0000-000000000000"

// resolveTerminalBackend decides how a session's terminal tab runs. The call
// is by environment identity, never by local path: a project whose
// EnvironmentID is empty, nil, or equal to this client's identity runs a
// local plugin pty in its directory; any other non-empty EnvironmentID is a
// remote daemon the app has no client for and reports as remote.
func (a *app) resolveTerminalBackend(sessionID string) (cwd string, remote bool, err error) {
	project := a.projectForTerminalSession(sessionID)
	if project == nil {
		return "", false, errors.New("No project for this session")
	}
	if project.Path == "" {
		return "", false, errors.New("No local shell for this session")
	}
	environment := project.EnvironmentID
	if environment == "" || environment == nilUUID {
		return project.Path, false, nil
	}
	if a.client != nil && environment == a.client.Identity().EnvironmentID {
		return project.Path, false, nil
	}
	return project.Path, true, nil
}

// projectForTerminalSession finds the workspace a session's terminal runs in:
// the selected detail when it matches, else the catalog entry.
func (a *app) projectForTerminalSession(sessionID string) *model.Project {
	var projectID string
	if a.detail != nil && a.detail.ID == sessionID && a.detail.ProjectID != "" {
		projectID = a.detail.ProjectID
	} else if session := a.summarySession(sessionID); session != nil {
		projectID = session.ProjectID
	} else if a.detail != nil && sessionID == "" {
		projectID = a.detail.ProjectID
	}
	if projectID == "" {
		if project := a.selectedProject(); project != nil {
			return project
		}
		return nil
	}
	return a.projectForID(projectID)
}
