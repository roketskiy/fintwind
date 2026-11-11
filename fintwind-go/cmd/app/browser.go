package main

import (
	"errors"
	"log"
	"net/url"
	"strings"

	"github.com/egoist/mygo"
)

type nativeBrowserTab struct {
	tabID      string
	sessionID  string
	address    string
	pendingURL string
	committed  string
	title      string
	loading    bool
	loadErr    string
	canBack    bool
	canFwd     bool
	view       *mygo.WebView
	creating   bool
	closed     bool
	gen        uint64
}

// Stable automation labels for the browser surface.
const (
	nativeBrowserLabel        = "native-browser"
	nativeBrowserAddressLabel = "native-browser-address"
	nativeBrowserTitleLabel   = "native-browser-title"
	nativeBrowserErrorLabel   = "native-browser-error"
)

// openNativeBrowser adds a tab and creates its web view off the render path.
// Window.NewWebView is safe from any goroutine (it hops to the main thread
// internally and reports errDestroyed after shutdown), so creation runs in
// the background; a window that closed first releases the fresh view through
// postOrReleaseOrDrop instead of leaking it. Main thread only.
func (a *app) openNativeBrowser(sessionID string) *nativeBrowserTab {
	entry := a.nativePanelsFor(sessionID)
	tab := &nativeBrowserTab{
		tabID:     newNativeTabID(),
		sessionID: sessionID,
		gen:       1,
	}
	entry.browsers = append(entry.browsers, tab)
	tab.creating = true
	gen := tab.gen
	tabID := tab.tabID
	win := a.win
	go func() {
		if win == nil {
			a.postOrReleaseOrDrop(nil, func() {
				a.finishNativeBrowserCreate(sessionID, tabID, gen, nil, errNativeWindowMissing)
			})
			return
		}
		view, err := win.NewWebView(mygo.WebViewOptions{})
		a.postOrReleaseOrDrop(func() {
			if view != nil {
				view.Destroy()
			}
		}, func() {
			a.finishNativeBrowserCreate(sessionID, tabID, gen, view, err)
		})
	}()
	return tab
}

// finishNativeBrowserCreate adopts a new web view or destroys it when the tab
// already moved on. Callback registration and any navigation happen here on
// the main thread, never in render. Runs inside a.post.
func (a *app) finishNativeBrowserCreate(sessionID, tabID string, gen uint64, view *mygo.WebView, err error) {
	tab := a.findNativeBrowserTab(sessionID, tabID, gen)
	if tab == nil {
		if view != nil {
			view.Destroy()
		}
		if err != nil {
			log.Println(err)
		}
		return
	}
	tab.creating = false
	if err != nil || view == nil {
		if err != nil {
			tab.loadErr = err.Error()
		} else {
			tab.loadErr = errNativeWindowMissing.Error()
		}
		entry, ok := a.native.sessions[sessionID]
		if ok && entry != nil {
			entry.removeNativeBrowserTab(tab)
			a.disposeNativeBrowser(tab)
		}
		return
	}
	tab.view = view
	page := view.Page()
	page.OnDidNavigate(func(navigatedURL string) {
		a.post(func() {
			a.applyNativeBrowserNavigate(sessionID, tabID, gen, navigatedURL)
		})
	})
	page.OnPageTitleUpdated(func(event *mygo.TitleEvent) {
		title := ""
		if event != nil {
			title = event.Title
		}
		a.post(func() {
			a.applyNativeBrowserTitle(sessionID, tabID, gen, title)
		})
	})
	page.OnDidFinishLoad(func() {
		a.post(func() {
			a.applyNativeBrowserSettled(sessionID, tabID, gen)
		})
	})
	page.OnDidFailLoad(func(loadErr *mygo.LoadError) {
		message := "Load failed"
		if loadErr != nil && loadErr.Description != "" {
			message = loadErr.Description
		}
		a.post(func() {
			a.applyNativeBrowserError(sessionID, tabID, gen, message)
		})
	})
	if tab.pendingURL != "" {
		pending := tab.pendingURL
		tab.pendingURL = ""
		_ = a.navigateNativeBrowser(tab, pending)
	}
}

// applyNativeBrowserNavigate syncs the address bar to a committed page URL.
// Late arrivals after close or a newer generation are dropped. Main thread.
func (a *app) applyNativeBrowserNavigate(sessionID, tabID string, gen uint64, navigatedURL string) {
	if tab := a.findNativeBrowserTab(sessionID, tabID, gen); tab != nil {
		tab.committed = navigatedURL
		tab.address = navigatedURL
		tab.loading = true
		tab.loadErr = ""
		a.refreshNativeBrowserHistory(tab)
	}
}

// applyNativeBrowserTitle records the page title. Main thread only.
func (a *app) applyNativeBrowserTitle(sessionID, tabID string, gen uint64, title string) {
	if tab := a.findNativeBrowserTab(sessionID, tabID, gen); tab != nil {
		tab.title = title
	}
}

func (a *app) applyNativeBrowserSettled(sessionID, tabID string, gen uint64) {
	if tab := a.findNativeBrowserTab(sessionID, tabID, gen); tab != nil {
		tab.loading = false
		tab.loadErr = ""
		a.refreshNativeBrowserHistory(tab)
	}
}

func (a *app) applyNativeBrowserError(sessionID, tabID string, gen uint64, message string) {
	if tab := a.findNativeBrowserTab(sessionID, tabID, gen); tab != nil {
		tab.loading = false
		tab.loadErr = message
		a.refreshNativeBrowserHistory(tab)
	}
}

// refreshNativeBrowserHistory echoes back/forward availability from the live
// page. Main thread only.
func (a *app) refreshNativeBrowserHistory(tab *nativeBrowserTab) {
	if tab == nil || tab.view == nil {
		return
	}
	page := tab.view.Page()
	tab.canBack = page.CanGoBack()
	tab.canFwd = page.CanGoForward()
}

func (a *app) navigateNativeBrowser(tab *nativeBrowserTab, rawURL string) error {
	if tab == nil || tab.closed {
		return errNativeBrowserClosed
	}
	checked, ok := nativeBrowserURL(rawURL)
	if !ok {
		tab.loadErr = "Only HTTP and HTTPS addresses open here"
		return errNativeBrowserScheme
	}
	tab.address = checked
	tab.loadErr = ""
	if tab.view == nil {
		tab.pendingURL = checked
		tab.loading = true
		return nil
	}
	tab.loading = true
	if err := tab.view.Page().LoadURL(checked); err != nil {
		tab.loading = false
		tab.loadErr = err.Error()
		return err
	}
	return nil
}

// stopNativeBrowser halts an in-flight load. Main thread only.
func (a *app) stopNativeBrowser(tab *nativeBrowserTab) {
	if tab == nil || tab.closed || tab.view == nil {
		return
	}
	tab.view.Page().Stop()
	tab.loading = false
}

// nativeBrowserBack, nativeBrowserForward and nativeBrowserReload drive the
// live page. They read history state the page reports. Main thread only.
func (a *app) nativeBrowserBack(tab *nativeBrowserTab) {
	if tab == nil || tab.closed || tab.view == nil {
		return
	}
	tab.view.Page().GoBack()
	tab.loading = true
}

func (a *app) nativeBrowserForward(tab *nativeBrowserTab) {
	if tab == nil || tab.closed || tab.view == nil {
		return
	}
	tab.view.Page().GoForward()
	tab.loading = true
}

func (a *app) nativeBrowserReload(tab *nativeBrowserTab) {
	if tab == nil || tab.closed || tab.view == nil {
		return
	}
	tab.loading = true
	tab.view.Page().Reload()
}

// openNativeBrowserExternal opens the committed page in the system browser.
// Only validated HTTP/HTTPS input leaves the app. Main thread only, called
// from a handled Clicked action.
func (a *app) openNativeBrowserExternal(tab *nativeBrowserTab) {
	if tab == nil || tab.closed {
		return
	}
	target := tab.committed
	if target == "" {
		target = tab.address
	}
	if checked, ok := nativeBrowserURL(target); ok {
		_ = mygo.Shell.OpenExternal(checked)
	}
}

// nativeBrowserURL accepts HTTP/HTTPS input only, adding a host check so a
// bare word never becomes a navigation.
func nativeBrowserURL(raw string) (string, bool) {
	trimmed := strings.TrimSpace(raw)
	if trimmed == "" {
		return "", false
	}
	parsed, err := url.Parse(trimmed)
	if err != nil || parsed.Hostname() == "" {
		return "", false
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return "", false
	}
	return parsed.String(), true
}

// findNativeBrowserTab locates a live tab by identity and generation.
// Anything closed, removed, or superseded returns nil so late callbacks
// release instead of applying.
func (a *app) findNativeBrowserTab(sessionID, tabID string, gen uint64) *nativeBrowserTab {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return nil
	}
	for _, tab := range entry.browsers {
		if tab.tabID == tabID && tab.gen == gen && !tab.closed {
			return tab
		}
	}
	return nil
}

// closeNativeBrowserTab closes the tab the user dismissed: exactly the view
// this file created and still holds is destroyed, once. Main thread only.
func (a *app) closeNativeBrowserTab(sessionID, tabID string) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	for _, tab := range entry.browsers {
		if tab.tabID != tabID {
			continue
		}
		entry.removeNativeBrowserTab(tab)
		a.disposeNativeBrowser(tab)
		return
	}
}

func (a *app) disposeNativeBrowser(tab *nativeBrowserTab) {
	if tab == nil || tab.closed {
		return
	}
	tab.closed = true
	if tab.view != nil {
		tab.view.Destroy()
		tab.view = nil
	}
	tab.creating = false
}

// errNativeWindowMissing reports a tab opened after the window went away.
var errNativeWindowMissing = errors.New("window is closed")

// errNativeBrowserClosed reports an action on a dismissed tab.
var errNativeBrowserClosed = errors.New("browser tab is closed")

// errNativeBrowserScheme reports a non-web address.
var errNativeBrowserScheme = errors.New("Only HTTP and HTTPS addresses open here")
