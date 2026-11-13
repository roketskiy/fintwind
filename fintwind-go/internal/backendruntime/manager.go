package backendruntime

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
)

type Emit func(session, runtime, kind string, payload any) error

type Manager struct {
	mu           sync.Mutex
	startMu      sync.Mutex
	runtimes     map[string]*actor
	ctx          context.Context
	cancel       context.CancelFunc
	root         string
	settingsPath string
	daily        bool
	store        *backendstore.Store
	emit         Emit
	// OnStart fires after a new actor takes session ownership, before its
	// loop runs. It lets the owner bind event state only for runtimes that
	// actually started; failed or duplicate starts never touch it.
	OnStart     func(sessionID, runtimeID string)
	OnStop      func(sessionID, runtimeID string)
	EventCursor func(sessionID, runtimeID string) (model.RuntimeEventCursor, bool)
}

type request struct {
	ctx     context.Context
	command backendstore.Object
	reply   chan outcome
}
type outcome struct {
	value backendstore.Object
	err   error
}

type actor struct {
	manager                             *Manager
	sessionID, runtimeID, nativeID      string
	process                             *backendengine.Process
	commands                            chan request
	done                                chan struct{}
	ctx                                 context.Context
	cancel                              context.CancelFunc
	events                              <-chan backendengine.Event
	snapshot                            backendstore.Object
	parts                               map[string]int
	reasoning                           map[string]int
	started                             bool
	turnID                              string
	dirty                               bool
	messages                            []backendstore.Object
	blocks                              []backendstore.Object
	turns                               []backendstore.Object
	forms                               map[string]map[string]bool
	approved                            map[string]bool
	tools                               map[string]string
	parent                              *actor
	child                               *childState
	children                            map[string]*childState
	pendingChildren                     []pendingChild
	requestScopes                       map[string]string
	backgroundBytes                     int
	backgroundGeneration                uint64
	backgroundRevision                  uint64
	backgroundCancel                    context.CancelFunc
	backgroundResults                   chan backgroundResult
	pendingChildStarts                  map[string]bool
	unresolvedChildEvents               map[string][]backendengine.Event
	unresolvedChildBytes                int
	otherBackground                     []model.BackgroundWorkSnapshot
	shells                              map[string]*shellState
	shellResults                        chan []shellResult
	shellReading                        bool
	goalResults                         chan goalReadResult
	goalReading                         bool
	goalRevision                        uint64
	goalChecked                         time.Time
	usageTotal, usageCache, usagePrompt uint64
	usageAuthoritative                  bool
	usageInitialized                    bool
}

func New(ctx context.Context, root, settingsPath string, daily bool, store *backendstore.Store, emit Emit) *Manager {
	ctx, cancel := context.WithCancel(ctx)
	return &Manager{runtimes: map[string]*actor{}, ctx: ctx, cancel: cancel, root: root, settingsPath: settingsPath, daily: daily, store: store, emit: emit}
}

func (m *Manager) Close() {
	m.cancel()
	m.mu.Lock()
	actors := make([]*actor, 0, len(m.runtimes))
	for _, actor := range m.runtimes {
		actors = append(actors, actor)
	}
	m.mu.Unlock()
	for _, actor := range actors {
		<-actor.done
	}
}

func (m *Manager) HasActive() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	for _, actor := range m.runtimes {
		select {
		case <-actor.done:
		default:
			return true
		}
	}
	return false
}

// Stop cancels one session runtime, waits for its own actor handle, and
// removes it from the map. It runs on a caller goroutine, never on the
// actor itself, so waiting for done cannot deadlock. Unknown sessions
// report false without an error.
func (m *Manager) Stop(sessionID string) (string, bool) {
	m.startMu.Lock()
	defer m.startMu.Unlock()
	m.mu.Lock()
	runtime, ok := m.runtimes[sessionID]
	if !ok {
		m.mu.Unlock()
		return "", false
	}
	delete(m.runtimes, sessionID)
	m.mu.Unlock()
	runtime.cancel()
	<-runtime.done
	if m.OnStop != nil {
		m.OnStop(sessionID, runtime.runtimeID)
	}
	return runtime.runtimeID, true
}

// workspaceRoot is the only directory engine working directories may live
// under: the isolated profile workspace, never the profile root itself, so
// data and home directories stay out of reach.
func (m *Manager) workspaceRoot() (string, error) {
	root, err := filepath.EvalSymlinks(m.root)
	if err != nil {
		return "", errors.New("isolated profile is not usable")
	}
	workspace := filepath.Join(root, "workspace")
	info, err := os.Stat(workspace)
	if err != nil || !info.IsDir() {
		return "", errors.New("isolated workspace is not available")
	}
	return workspace, nil
}

func withinWorkspace(workspace, cwd string) error {
	if cwd == "" {
		return errors.New("engine workspace is required")
	}
	canonical, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return errors.New("engine workspace is not accessible")
	}
	rel, err := filepath.Rel(workspace, canonical)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return errors.New("engine workspace outside isolated workspace")
	}
	return nil
}
func (m *Manager) Handle(ctx context.Context, sessionID, runtimeID string, command backendstore.Object) (backendstore.Object, error) {
	kind := command.String("type")
	if kind == "start" {
		return m.start(ctx, sessionID, runtimeID, command)
	}
	m.mu.Lock()
	runtime := m.runtimes[sessionID]
	m.mu.Unlock()
	if kind == "attachSession" {
		var id any
		if runtime != nil {
			select {
			case <-runtime.done:
			default:
				id = runtime.runtimeID
			}
		}
		return backendstore.Reply("sessionRuntime", backendstore.Fields(map[string]any{"runtimeId": id, "supportsSteer": false})), nil
	}
	if runtime == nil {
		return nil, errors.New("session runtime missing")
	}
	if runtimeID != "" && runtimeID != "00000000-0000-0000-0000-000000000000" && runtimeID != runtime.runtimeID {
		return nil, errors.New("runtime identity mismatch")
	}
	reply := make(chan outcome, 1)
	select {
	case runtime.commands <- request{ctx: ctx, command: command, reply: reply}:
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-runtime.done:
		return nil, errors.New("runtime closed")
	}
	select {
	case result := <-reply:
		return result.value, result.err
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-runtime.done:
		return nil, errors.New("runtime closed")
	}
}

func (m *Manager) start(ctx context.Context, sessionID, runtimeID string, command backendstore.Object) (backendstore.Object, error) {
	m.startMu.Lock()
	defer m.startMu.Unlock()
	if m.store.Removed(sessionID) {
		return nil, errors.New("session was removed")
	}
	if runtimeID == "" || runtimeID == "00000000-0000-0000-0000-000000000000" {
		return nil, errors.New("runtime identity required")
	}
	m.mu.Lock()
	existing := m.runtimes[sessionID]
	m.mu.Unlock()
	if existing != nil {
		select {
		case <-existing.done:
			m.mu.Lock()
			delete(m.runtimes, sessionID)
			m.mu.Unlock()
			existing = nil
		default:
			if existing.runtimeID != runtimeID {
				return nil, errors.New("session already has a runtime")
			}
			return backendstore.Reply("started", backendstore.Fields(map[string]any{"supportsSteer": false})), nil
		}
	}
	m.mu.Lock()
	for id, runtime := range m.runtimes {
		select {
		case <-runtime.done:
			delete(m.runtimes, id)
		default:
		}
	}
	count := len(m.runtimes)
	m.mu.Unlock()
	if !m.daily && count >= 4 {
		return nil, errors.New("isolated engine runtime limit reached")
	}
	options, err := backendstore.Decode(command["options"])
	if err != nil {
		return nil, err
	}
	var resume struct {
		Provider  string `json:"provider"`
		SessionID string `json:"sessionId"`
	}
	if raw := options["providerCursor"]; len(raw) > 0 && string(raw) != "null" {
		if err := json.Unmarshal(raw, &resume); err != nil || resume.Provider != "openCode" || resume.SessionID == "" {
			return nil, errors.New("invalid native resume cursor")
		}
	}
	if !m.daily {
		workspace, err := m.workspaceRoot()
		if err != nil {
			return nil, err
		}
		if err := withinWorkspace(workspace, options.String("cwd")); err != nil {
			return nil, err
		}
	}
	process, err := backendengine.Start(ctx, options.String("binary"), m.root, options.String("cwd"), m.settingsPath, m.daily)
	if err != nil {
		return nil, err
	}
	native := resume.SessionID
	if native != "" {
		err = process.Client.ResumeSession(ctx, native)
	} else {
		native, err = process.Client.CreateSession(ctx)
	}
	if err != nil {
		_ = process.Close()
		return nil, err
	}
	agent := "build"
	if options.String("interactionMode") == "plan" {
		agent = "plan"
	}
	if err := process.Client.Configure(ctx, native, options.String("model"), options.String("reasoningEffort"), agent); err != nil {
		_ = process.Close()
		return nil, err
	}
	actorCtx, cancel := context.WithCancel(m.ctx)
	events, err := process.Client.Events(actorCtx)
	if err != nil {
		cancel()
		_ = process.Close()
		return nil, err
	}
	actor := &actor{manager: m, sessionID: sessionID, runtimeID: runtimeID, nativeID: native, process: process, commands: make(chan request, 16), done: make(chan struct{}), ctx: actorCtx, cancel: cancel, events: events, parts: map[string]int{}, reasoning: map[string]int{}, forms: map[string]map[string]bool{}, approved: map[string]bool{}}
	if resume.SessionID != "" {
		payload, loadErr := m.store.Hydrate(ctx, sessionID)
		if loadErr != nil {
			cancel()
			_ = process.Close()
			return nil, loadErr
		}
		actor.snapshot, err = backendstore.Decode(payload["session"])
		if err != nil {
			cancel()
			_ = process.Close()
			return nil, errors.New("saved task required to resume native session")
		}
		if err := actor.loadSnapshot(); err != nil {
			cancel()
			_ = process.Close()
			return nil, err
		}
		if err := actor.reconcile(ctx); err != nil {
			cancel()
			_ = process.Close()
			return nil, err
		}
	}
	m.mu.Lock()
	m.runtimes[sessionID] = actor
	onStart := m.OnStart
	m.mu.Unlock()
	if onStart != nil {
		onStart(sessionID, runtimeID)
	}
	go actor.run()
	return backendstore.Reply("started", backendstore.Fields(map[string]any{"supportsSteer": false})), nil
}
