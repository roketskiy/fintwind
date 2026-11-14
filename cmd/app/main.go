package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sync"
	"time"

	"fintwind-go/internal/composerpaste"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/diagramview"
	"fintwind-go/internal/model"

	"github.com/ELY-Open-Source/CrychicUI/sound"
	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/markdown"
	"github.com/lobehub/lobe-mygo/pkg/feedback"
)

type mode int

const (
	modeSessions mode = iota
	modeSourceControl
	modeScheduled
	modeUsage
	modeSettings
)

type app struct {
	win    *mygo.Window
	proc   *daemon.Process
	client *daemon.Client

	done            chan struct{}
	closeOnce       sync.Once
	lifetime        context.Context
	cancelLifetime  context.CancelFunc
	native          nativePanelState
	live            map[string]*sessionState
	connection      connectionState
	applyingEvents  bool
	transcriptDirty bool
	sidebarDirty    bool
	snapshots       snapshotWriter
	branches        map[string]branchState
	tabStripScroll  ui.ScrollState
	tabRevealID     string

	projects []model.Project
	sessions []model.Session
	byID     map[string]*model.Session
	rows     []sidebarRow

	selectedID        string
	openTabs          []string
	lastProject       string
	detail            *model.Session
	generation        int
	sessionLoadCancel context.CancelFunc
	loading           bool
	detailLoading     bool
	status            string
	fatalErr          string
	detailErr         string

	sidebarOpen bool
	rightOpen   bool

	// Panel widths in DIPs. They live only in memory: Rust persists the
	// sidebar and right panel widths, but there is no settings write
	// channel yet, so nothing is stored.
	sidebarWidth         float32
	rightPanelWidthValue float32
	fileTreeWidth        float32
	// Every secondary column stretches and collapses like the sessions
	// sidebar; each keeps its own width.
	settingsSecWidth float32
	scSecWidth       float32
	mode             mode
	collapsed        map[string]bool
	visible          map[string]int

	sidebarList    ui.ListState
	messageList    ui.ListState
	md             markdown.Options
	mdUser         markdown.Options
	mdCompact      markdown.Options
	mdReasoning    markdown.Options
	mdReady        bool
	diagrams       map[[32]byte]*diagramState
	diagramWorkers chan struct{}
	diagramClock   uint64
	diagramViewer  diagramview.Viewer
	draft          string
	composerCard   ui.Handle
	composerPaste  composerpaste.State
	composerWidths [5]composerLabelWidth
	surfaces       []string
	activeSurface  int
	settings       settingsState
	usage          usageState
	review         reviewState
	sourceControl  sourceControlState
	remote         remoteState
	help           helpState
	optionsSync    map[string]*sessionOptionsSync
	sessionActions sessionActionState
	modelDiscovery modelDiscoveryState
	appearance     appearanceState
	autoCompacted  map[string]bool
	contextPanel   contextPanelState
	capsule        capsuleState

	transcriptRows     []transcriptRow
	expandedTurns      map[string]bool
	clusterDisclosure  map[int]int
	expandedActivities map[string]bool

	expandedUserMessages map[string]bool
	copiedMessages       map[string]uint64
	copiedGeneration     uint64
	expandedCompaction   map[string]bool
	expandedChangedFiles map[string]bool
	collapsedQueued      map[string]bool

	pendingPermission *pendingPermission
	pendingUserInput  *userInputState
	// sessionAttention holds a session's actionable state (question or
	// permission) while its runtime waits, even for a background tab.
	sessionAttention map[string]string
	tabUnread        map[string]bool

	treeRows             []treeRow
	treeExpanded         map[string]bool
	runtimes             map[string]string
	projectlessRoot      string
	projectMenuOpen      bool
	emptyProjectMenuOpen bool
	branchLabel          string
	unsavedDraft         string

	selectedFile        string
	fileLines           []string
	fileLoading         bool
	fileErr             string
	navHover            int
	navHistory          []string
	navIndex            int
	accessMenuOpen      bool
	modelMenuOpen       bool
	modelQuery          string
	editMessage         *messageEditState
	editDraft           string
	detailRaw           map[string]any
	providersIndex      int
	providersTab        string
	skillsIndex         int
	mcpIndex            int
	modelProbe          *providerProbe
	modelProbeLoad      bool
	modelHighlight      int
	modelListScroll     ui.ScrollState
	modelMenuJustOpened bool
	focusComposerOnce   bool
	// modelPickerTab is the draw sidebar tab: "favorites" or
	// "provider:<label>". It persists across opens but every open resets
	// it to the current model's provider tab, mirroring
	// selected_model_picker_tab.
	modelPickerTab          string
	modelPickerWasSearching bool
	// favoriteModels is the starred catalog ids in starred order, mirroring
	// state.favorite_models (the Favorites tab lists them in this order).
	favoriteModels       []string
	renamingSession      string
	renameSelectAll      bool
	renameDraft          string
	projectRemoveConfirm string
	forkingResponses     map[string]bool
	paletteOpen          bool
	paletteQuery         string
	paletteIndex         int
	settingsRaw          map[string]any
	settingsLoaded       bool
	pendingAttachments   []draftAttachment
	// Staged text quotes above the composer, folded into the next prompt.
	pendingReferences []composerReference
	// Composer autocomplete (/ and @) over prefetched indexes. Render reads
	// only these slices; refreshAutocompleteSources fills them off-thread.
	slashIndex            []slashCommand
	mentionFiles          []mentionFile
	autocompleteDir       string
	autocompleteLoading   bool
	autocompleteGen       uint64
	autocompleteKey       string
	autocompleteHighlight int
	autocompleteDismissed bool
	autocompleteOpen      bool
	autocompleteMemoKey   string
	autocompleteMemoRows  []autocompleteRow
	// Render-time caret in runes plus a pending caret applied next frame
	// after a TextKey insertion (SetText drops the caret to the end).
	composerCaretStart int
	composerCaretEnd   int
	pendingCaret       *int
	tabDragID          string
	tabDragDX          float32
}

type permissionOption struct {
	ID    string `json:"id"`
	Label string `json:"label"`
	Allow bool   `json:"allow"`
}

// pendingPermission is the provider's live approval request.
type pendingPermission struct {
	RequestID string
	Title     string
	Detail    string
	Options   []permissionOption
	Response  sessionReplyState
}

// userInputState is an agent's live question flow.
type userInputState struct {
	RequestID  string
	Questions  []userInputQuestion
	Index      int
	Selections map[int][]string
	Custom     []string
	Collapsed  bool
	Response   sessionReplyState
}

type userInputQuestion struct {
	ID          string            `json:"id"`
	Header      string            `json:"header"`
	Question    string            `json:"question"`
	Options     []userInputOption `json:"options"`
	MultiSelect bool              `json:"multiSelect"`
}

type userInputOption struct {
	Label       string  `json:"label"`
	Description *string `json:"description"`
}

func main() {
	sound.SetEnabled(false)
	redirectCrashLog()
	dataDir := flag.String("data-dir", defaultDataDir(), "daemon data directory")
	daemonPath := flag.String("daemon", defaultDaemonPath(), "path to fintwind-daemon.exe")
	profile := flag.String("profile", "agent", "launch profile: daily or agent")
	preferencesDir := flag.String("preferences-dir", "", "client preferences directory")
	connect := flag.Bool("connect", false, "connect to the launcher-owned daemon")
	flag.Parse()
	if !*connect && flag.NFlag() == 0 {
		root := os.Getenv("LOCALAPPDATA")
		if root == "" {
			log.Fatal("LOCALAPPDATA is required")
		}
		*dataDir = filepath.Join(root, "Fintwind")
		*preferencesDir = filepath.Join(root, "Fintwind Go")
		*profile = "daily"
		address, token, stop, err := startReleaseDaemon(*daemonPath, *dataDir)
		if err != nil {
			log.Fatal(err)
		}
		defer stop()
		_ = os.Setenv("FINTWIND_MYGO_PROFILE", "daily")
		_ = os.Setenv("FINTWIND_MYGO_DAEMON_ADDRESS", address)
		_ = os.Setenv("FINTWIND_MYGO_DAEMON_TOKEN", token)
		*connect = true
	}
	if *profile != "daily" && *profile != "agent" {
		log.Fatal("unsupported launch profile")
	}
	if *profile == "daily" && (!*connect || os.Getenv("FINTWIND_MYGO_PROFILE") != "daily") {
		log.Fatal("use MyGo.cmd to open the real Fintwind profile")
	}
	if *profile == "agent" {
		if err := daemon.CheckIsolatedDataDir(*dataDir); err != nil {
			log.Fatal(err)
		}
	}
	address, token := "", ""
	if *connect {
		address, token = os.Getenv("FINTWIND_MYGO_DAEMON_ADDRESS"), os.Getenv("FINTWIND_MYGO_DAEMON_TOKEN")
		if address == "" || token == "" {
			log.Fatal("launcher daemon connection is missing")
		}
	}
	_ = os.Unsetenv("FINTWIND_MYGO_DAEMON_ADDRESS")
	_ = os.Unsetenv("FINTWIND_MYGO_DAEMON_TOKEN")
	prefsDir := *preferencesDir
	if prefsDir == "" {
		prefsDir = *dataDir
	}

	layout := loadLocalSettingsFrom(*dataDir, prefsDir)
	lifetime, cancelLifetime := context.WithCancel(context.Background())
	defer cancelLifetime()
	a := &app{
		done:                 make(chan struct{}),
		lifetime:             lifetime,
		cancelLifetime:       cancelLifetime,
		live:                 make(map[string]*sessionState),
		sidebarOpen:          true,
		rightOpen:            false,
		sidebarWidth:         layout.sidebar,
		rightPanelWidthValue: layout.rightPanel,
		fileTreeWidth:        layout.fileTree,
		settingsSecWidth:     settingsWidth,
		scSecWidth:           scSecondaryWidth,
		loading:              true,
		mode:                 modeSessions,
		treeExpanded:         map[string]bool{},
		runtimes:             map[string]string{},
		sessionAttention:     map[string]string{},
		tabUnread:            map[string]bool{},
		forkingResponses:     map[string]bool{},
		activeSurface:        -1,
		navHover:             -1,
		navIndex:             -1,
		settings: settingsState{
			page:              "general",
			autoCompactThresh: "155000",
		},
	}
	a.initPreferences(layout)
	mygo.App.WhenReady(func() {
		a.win = mygo.NewWindow(mygo.WindowOptions{
			Title:          "fintwind",
			Width:          1440,
			Height:         900,
			MinWidth:       980,
			MinHeight:      620,
			TitleBarStyle:  mygo.TitleBarHidden,
			TitleBarHeight: topbarHeight,
			// The framework caption is hidden: the top bar draws its own
			// minimize/maximize/close with full color control.
			HideWindowButtons: true,
			BackgroundColor:   "#141414",
			Content:           ui.View(a.view),
		})
		a.win.OnClosed(func() {
			a.closePreferences()
			a.flushSnapshotsOnClose()
			a.closeOnce.Do(func() { close(a.done) })
			a.cancelLifetime()
			a.closeNativePanels()
			if a.client != nil {
				a.client.Close()
			}
			if a.proc != nil {
				a.proc.Stop()
			}
		})
		go a.bootConnection(*daemonPath, *dataDir, address, token)
		go a.watchAppearance()
	})
	runErr := mygo.App.Run()
	a.closePreferences()
	flushCtx, flushCancel := context.WithTimeout(context.Background(), 3*time.Second)
	if err := a.flushPreferences(flushCtx); err != nil {
		log.Printf("save local preferences while closing: %v", err)
	}
	flushCancel()
	if runErr != nil {
		log.Fatal(runErr)
	}
}

func defaultDataDir() string {
	if root := os.Getenv("LOCALAPPDATA"); root != "" {
		return filepath.Join(root, "Fintwind Go", "data")
	}
	return filepath.Join(".", "data")
}

func defaultDaemonPath() string {
	if executable, err := os.Executable(); err == nil {
		root := filepath.Dir(executable)
		candidates := []string{
			filepath.Join(root, "fintwind-daemon.exe"),
			filepath.Join(root, "..", "target", "release", "fintwind-daemon.exe"),
			filepath.Join(root, "..", "..", "target", "release", "fintwind-daemon.exe"),
		}
		for _, candidate := range candidates {
			if _, err := os.Stat(candidate); err == nil {
				return candidate
			}
		}
	}
	candidates := []string{
		filepath.Join("..", "target", "release", "fintwind-daemon.exe"),
		filepath.Join("..", "..", "target", "release", "fintwind-daemon.exe"),
	}
	for _, candidate := range candidates {
		if _, err := os.Stat(candidate); err == nil {
			return candidate
		}
	}
	return candidates[0]
}

func (a *app) boot(daemonPath, dataDir string) {
	a.bootConnection(daemonPath, dataDir, "", "")
}

func (a *app) bootConnection(daemonPath, dataDir, address, token string) {
	ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
	defer cancel()
	collapsedQueued := a.loadQueuedCollapsed()
	if err := a.onUI(func() error { a.collapsedQueued = collapsedQueued; return nil }); err != nil {
		return
	}
	a.post(func() { a.status = tr("Starting the daemon") })
	var proc *daemon.Process
	var err error
	if address == "" {
		proc, err = daemon.Start(ctx, daemonPath, dataDir)
		if err != nil {
			a.fail(fmt.Errorf("start daemon: %w", err))
			return
		}
		if err := a.onUI(func() error { a.proc = proc; return nil }); err != nil {
			proc.Stop()
			return
		}
		address, token = proc.Address, proc.Token
	}
	client, err := daemon.Dial(ctx, address, token)
	if err != nil {
		proc.Stop()
		a.fail(fmt.Errorf("connect to daemon: %w", err))
		return
	}
	if err := a.onUI(func() error {
		a.client = client
		identity := client.Identity()
		a.connection = connectionState{phase: "online", environmentID: identity.EnvironmentID, instanceID: identity.InstanceID}
		a.status = tr("Loading sessions")
		a.ensureSettingsLoaded()
		return nil
	}); err != nil {
		client.Close()
		proc.Stop()
		return
	}
	go a.consumeConnection(client, address, token)
	payload, err := client.Request(ctx, map[string]any{"type": "loadTaskState"})
	if err != nil {
		proc.Stop()
		a.fail(fmt.Errorf("load sessions: %w", err))
		return
	}
	var state model.TaskState
	if err := json.Unmarshal(payload, &state); err != nil {
		proc.Stop()
		a.fail(fmt.Errorf("decode sessions: %w", err))
		return
	}
	a.post(func() {
		a.projects = state.Projects
		a.sessions = state.Sessions
		a.projectlessRoot = state.ProjectlessRoot
		a.byID = make(map[string]*model.Session, len(state.Sessions))
		for i := range a.sessions {
			a.byID[a.sessions[i].ID] = &a.sessions[i]
		}
		a.rows = a.rows[:0]
		a.rebuildRows()
		a.loading = false
		a.status = ""
		if len(a.projects) > 0 {
			a.lastProject = a.projects[0].ID
		}
		if latest := a.latestSessionID(); latest != "" {
			a.openSession(latest)
		}
	})
}

func (a *app) fail(err error) {
	log.Println(err)
	a.post(func() {
		a.loading = false
		a.fatalErr = err.Error()
	})
}

func (a *app) post(update func()) {
	if dispatchHeadless(a, update) {
		return
	}
	if a.win == nil {
		return
	}
	select {
	case <-a.done:
		return
	default:
	}
	a.win.Update(func() {
		select {
		case <-a.done:
			return
		default:
		}
		a.collectSettingsFeedback()
		update()
		a.collectSettingsFeedback()
	})
}

func (a *app) latestSessionID() string {
	var latest *model.Session
	for i := range a.sessions {
		session := &a.sessions[i]
		if session.Archived {
			continue
		}
		if latest == nil || sessionStamp(session) > sessionStamp(latest) {
			latest = session
		}
	}
	if latest == nil {
		return ""
	}
	return latest.ID
}

func (a *app) openSession(id string) {
	if id == "" {
		return
	}
	if id == a.selectedID && a.detail != nil {
		return
	}
	a.recordHistory(id)
	a.loadSession(id)
}

// recordHistory appends a visited session, dropping any forward entries.
func (a *app) recordHistory(id string) {
	if a.navIndex >= 0 && a.navIndex < len(a.navHistory) && a.navHistory[a.navIndex] == id {
		return
	}
	if a.navIndex+1 < len(a.navHistory) {
		a.navHistory = a.navHistory[:a.navIndex+1]
	}
	a.navHistory = append(a.navHistory, id)
	a.navIndex = len(a.navHistory) - 1
}

// navigateHistory moves back or forward in the visited sessions.
func (a *app) navigateHistory(delta int) {
	next := a.navIndex + delta
	if next < 0 || next >= len(a.navHistory) {
		return
	}
	a.navIndex = next
	a.loadSession(a.navHistory[next])
}

func (a *app) advanceSessionGeneration() {
	if a.sessionLoadCancel != nil {
		a.sessionLoadCancel()
		a.sessionLoadCancel = nil
	}
	a.generation++
}

func (a *app) loadSession(id string) {
	if id == "" {
		return
	}
	if id == a.selectedID && a.detail != nil {
		return
	}
	a.rememberSession()
	a.switchNativeSession(a.selectedID, id)
	a.advanceSessionGeneration()
	generation := a.generation
	a.selectedID = id
	delete(a.tabUnread, id)
	a.addOpenTab(id)
	// A folded group would hide the task just opened; unfold its project and
	// grow the visible count so the selection stays visible in the sidebar,
	// mirroring request_session_activation's reveal on session change.
	a.revealSidebarSession(id)
	a.detail = nil
	a.detailRaw = nil
	a.draft = ""
	a.pendingAttachments = nil
	a.pendingReferences = nil
	a.pendingPermission = nil
	a.pendingUserInput = nil
	a.editMessage = nil
	a.editDraft = ""
	a.selectedFile = ""
	a.fileLines = nil
	a.fileLoading = false
	a.fileErr = ""
	a.autocompleteKey = ""
	a.autocompleteHighlight = 0
	a.autocompleteDismissed = false
	a.autocompleteOpen = false
	a.composerCaretStart = 0
	a.composerCaretEnd = 0
	a.pendingCaret = nil
	a.modelMenuOpen = false
	a.accessMenuOpen = false
	a.detailLoading = true
	a.detailErr = ""
	if a.restoreSession(id) {
		a.ensureContextSources()
		a.detailLoading = false
		a.rebuildTranscript()
		a.refreshTree()
		a.refreshBranch()
		return
	}
	client := a.client
	if client == nil {
		return
	}
	var projectPath string
	if summary := a.summarySession(id); summary != nil {
		if project := a.projectForID(summary.ProjectID); project != nil {
			projectPath = project.Path
		}
	}
	binary := a.settingsStr("opencode_binary_path", "opencode")
	ctx, cancel := context.WithTimeout(a.lifetime, 90*time.Second)
	a.sessionLoadCancel = cancel
	go func() {
		defer cancel()
		payload, err := client.Request(ctx, map[string]any{
			"type":          "hydrateSession",
			"sessionId":     id,
			"environmentId": "00000000-0000-0000-0000-000000000000",
		})
		if err != nil {
			a.post(func() {
				if generation != a.generation {
					return
				}
				a.detailLoading = false
				a.detailErr = err.Error()
			})
			return
		}
		var detail model.SessionDetail
		if err := json.Unmarshal(payload, &detail); err != nil {
			a.post(func() {
				if generation != a.generation {
					return
				}
				a.detailLoading = false
				a.detailErr = err.Error()
			})
			return
		}
		if session := detail.Session; session != nil && session.NativeSessionID != nil && *session.NativeSessionID != "" && len(session.Messages) == 0 && len(session.TranscriptBlocks) == 0 && len(session.Turns) == 0 {
			directory := projectPath
			if session.Workspace.Kind == "worktree" && session.Workspace.Path != "" {
				directory = session.Workspace.Path
			}
			var native struct {
				Transcript model.NativeTranscript `json:"transcript"`
			}
			var response json.RawMessage
			if directory == "" {
				err = errors.New("native session workspace is unavailable")
			} else {
				response, err = client.Request(ctx, map[string]any{"type": "fetchNativeTranscript", "binary": binary, "directory": directory, "sessionId": *session.NativeSessionID})
				if err == nil {
					err = json.Unmarshal(response, &native)
				}
			}
			if err != nil {
				a.post(func() {
					if generation == a.generation {
						a.detailLoading = false
						a.detailErr = err.Error()
					}
				})
				return
			}
			session.Messages, session.TranscriptBlocks, session.Turns = native.Transcript.Messages, native.Transcript.Blocks, native.Transcript.Turns
		}
		rawSession := decodeRawSession(payload)
		var usageSession, usageDirectory string
		if session := detail.Session; session != nil && session.NativeSessionID != nil && *session.NativeSessionID != "" && (session.ContextUsage == nil || !session.ContextUsage.Measured) {
			usageSession = *session.NativeSessionID
			usageDirectory = projectPath
			if session.Workspace.Kind == "worktree" && session.Workspace.Path != "" {
				usageDirectory = session.Workspace.Path
			}
		}
		var usageCursor *model.RuntimeEventCursor
		if err := a.onUI(func() error {
			if generation != a.generation {
				return context.Canceled
			}
			defer func() {
				if a.detail != nil && a.detail.RuntimeEventCursor != nil {
					cursor := *a.detail.RuntimeEventCursor
					usageCursor = &cursor
				}
			}()
			a.detailLoading = false
			if a.restoreSession(id) {
				a.ensureContextSources()
				a.rebuildTranscript()
				a.refreshTree()
				a.refreshBranch()
				return nil
			}
			a.detailRaw = rawSession
			a.preserveLocalSessionFields(detail.Session)
			if detail.Session != nil {
				a.detail = detail.Session
				a.syncSummary(detail.Session)
				if detail.Session.ProjectID != "" {
					a.lastProject = detail.Session.ProjectID
				}
			}
			a.expandedTurns = map[string]bool{}
			a.clusterDisclosure = map[int]int{}
			a.expandedActivities = map[string]bool{}
			a.expandedUserMessages = map[string]bool{}
			a.copiedMessages = map[string]uint64{}
			a.expandedCompaction = map[string]bool{}
			a.expandedChangedFiles = map[string]bool{}
			a.rebuildTranscript()
			a.refreshTree()
			a.refreshBranch()
			a.ensureContextSources()
			a.messageList = ui.ListState{FollowEnd: true}
			a.rememberSession()
			return nil
		}); err != nil || usageSession == "" || usageDirectory == "" {
			return
		}
		data, err := client.Request(ctx, map[string]any{"type": "fetchNativeUsage", "binary": binary, "directory": usageDirectory, "sessionId": usageSession})
		if err != nil {
			return
		}
		var result struct {
			Usage *model.ContextUsage `json:"usage"`
		}
		if json.Unmarshal(data, &result) != nil || result.Usage == nil {
			return
		}
		a.post(func() {
			if generation != a.generation || a.client != client || a.detail == nil || a.detail.ID != id || a.detail.Busy() || a.detail.NativeSessionID == nil || *a.detail.NativeSessionID != usageSession {
				return
			}
			currentCursor := a.detail.RuntimeEventCursor
			if (usageCursor == nil) != (currentCursor == nil) || usageCursor != nil && *usageCursor != *currentCursor {
				return
			}
			if old := a.detail.ContextUsage; old != nil {
				if old.Measured {
					return
				}
				result.Usage.Window, result.Usage.Composition = old.Window, old.Composition
				result.Usage.FirstTokenMs, result.Usage.FirstTokenTurns = old.FirstTokenMs, old.FirstTokenTurns
			}
			a.detail.ContextUsage = result.Usage
		})
	}()
}

// syncSummary copies hydrated list-facing fields back into the task list so
// the sidebar and tabs show the provider's title and the settled status.
func (a *app) syncSummary(session *model.Session) {
	if session == nil {
		return
	}
	target := a.summarySession(session.ID)
	if target == nil {
		return
	}
	target.Title = session.Title
	target.AutoTitle = session.AutoTitle
	target.Model = session.Model
	target.Workspace = session.Workspace
	target.Status = session.Status
	target.UpdatedAt = session.UpdatedAt
	target.LastReplyAt = session.LastReplyAt
	target.Archived = session.Archived
}

func (a *app) view(c *ui.Context) {
	a.diagramClock++ // diagram cache recency stamp for this frame
	if c.Shortcut(ui.Ctrl, ui.KeyK) {
		a.paletteOpen = !a.paletteOpen
		a.paletteQuery = ""
		a.paletteIndex = 0
	}
	if c.Shortcut(ui.Ctrl, ui.KeyU) {
		a.toggleContextPanel()
	}
	a.applyAppearance()
	c.SetTheme(uiText(c))
	if a.mode != modeUsage {
		a.usage.UI.rangeMenu = false
	}
	if !a.mdReady {
		a.md = newMarkdownOptions(c, 13.5, 21, 11.5, 17.5)
		a.mdUser = newMarkdownOptions(c, 14, 20, 11.5, 17.5)
		a.mdCompact = newMarkdownOptions(c, 11.5, 17, 10.5, 16)
		a.mdReasoning = newMarkdownOptions(c, 11.5, 17, 10.5, 16)
		// Mermaid renders as a diagram only in transcript message bodies;
		// reasoning and auxiliary markdown surfaces keep plain code blocks.
		for _, options := range []*markdown.Options{&a.md, &a.mdUser} {
			options.Code = map[string]func(*ui.Context, *markdown.CodeBlockView){"mermaid": a.renderDiagram, "markdown": a.renderMarkdownCode}
		}
		// Transcript messages share one selection per message row (like the
		// source's transcript selection): the row column is Selectable and
		// carries the unified menu, so these views leave selection to it.
		a.md.ShareSelection = true
		a.mdUser.ShareSelection = true
		if theme := a.mdReasoning.Theme; theme != nil {
			theme.Text = palette.textSecondary
			theme.Strong = palette.textSecondary
			theme.Muted = palette.textTertiary
		}
		a.mdReady = true
	}
	ui.Column(c).Fill().Background(palette.rail).Children(func() {
		a.topBar(c)
		ui.Row(c).FillWidth().Grow(1).AlignItems(ui.Stretch).Children(func() {
			a.modeRail(c)
			switch a.mode {
			case modeSessions:
				if a.sidebarOpen {
					a.sidebar(c)
				}
				workspaceFrame(c, func() {
					ui.Row(c).FillWidth().Grow(1).MinHeight(0).Children(func() {
						a.sessionsView(c)
						if a.rightOpen {
							a.rightPanel(c)
						}
					})
				})
			case modeSettings:
				if a.sidebarOpen {
					a.settingsSecondary(c)
				}
				workspaceFrame(c, func() { a.settingsContent(c) })
			case modeScheduled:
				workspaceFrame(c, func() { a.scheduledSettings(c) })
			case modeUsage:
				workspaceFrame(c, func() { a.usageView(c) })
			default:
				a.mainArea(c)
			}
		})
		a.commandPaletteLayer(c)
		a.sessionDeleteConfirmLayer(c)
		a.helpLayer(c)
		a.diagramViewer.Layer(c, diagramLabels())
	})
	a.collectSettingsFeedback()
	for _, item := range a.settings.feedback.pending {
		feedback.ShowToast(c, item)
	}
	a.settings.feedback.pending = nil
	feedback.ToastHost(c, feedback.ToastHostProps{})
}

func workspaceFrame(c *ui.Context, body func()) {
	outer := ui.Column(c).Grow(1).MinWidth(0).FillHeight().Padding(5).
		Background(palette.rail.Mix(palette.surface, 0.45)).Radius(workspaceRadius).Clip()
	workspaceShadow(outer).Children(func() {
		ui.Column(c).FillWidth().Grow(1).MinHeight(0).Background(palette.surface).
			Border(1, palette.borderStrong).Radius(workspaceRadius - 5).Clip().Children(body)
	})
}

func (a *app) mainArea(c *ui.Context) {
	switch a.mode {
	case modeSessions:
		a.sessionsView(c)
	case modeSettings:
		a.placeholder(c, "Settings", "settings")
	case modeSourceControl:
		a.sourceControlView(c)
	case modeScheduled:
		a.scheduledSettings(c)
	case modeUsage:
		a.usageView(c)
	default:
		a.sessionsView(c)
	}
}

func (a *app) placeholder(c *ui.Context, title, iconName string) {
	ui.Column(c).Grow(1).MinWidth(0).FillHeight().Background(palette.surface).
		Radius(workspaceRadius, 0, 0, workspaceRadius).Clip().Children(func() {
		ui.Row(c).FillWidth().Height(44).Padding(0, 16, 0, 16).AlignItems(ui.Center).Children(func() {
			ui.Text(c, title).FontSize(uiPx(14)).Bold().TextColor(palette.text)
		})
		ui.Column(c).FillWidth().Grow(1).Center().Gap(10).Children(func() {
			icon(c, iconName, 28, palette.textGhost)
			ui.Text(c, tr("Not ported yet")).FontSize(uiPx(13)).TextColor(palette.textMuted)
		})
	})
}

func (a *app) selectedProject() *model.Project {
	projectID := a.lastProject
	if a.detail != nil && a.detail.ProjectID != "" {
		projectID = a.detail.ProjectID
	}
	for i := range a.projects {
		if a.projects[i].ID == projectID {
			return &a.projects[i]
		}
	}
	return nil
}

func (a *app) modelLabel() string {
	if a.detail == nil || a.detail.Model == nil || *a.detail.Model == "" {
		return "OpenCode"
	}
	return *a.detail.Model
}
