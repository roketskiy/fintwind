package backend

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"log"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"

	"fintwind-go/internal/backendattachments"
	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendevents"
	"fintwind-go/internal/backendmcpauth"
	"fintwind-go/internal/backendquota"
	"fintwind-go/internal/backendruntime"
	"fintwind-go/internal/backendservices"
	"fintwind-go/internal/backendskills"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/backendusage"
	"fintwind-go/internal/backendworkspace"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

const maxFrame = daemon.MaxWireMessageBytes

type Identity struct {
	EnvironmentID string `json:"environmentId"`
	InstanceID    string `json:"instanceId"`
}

type Server struct {
	Store            *backendstore.Store
	Identity         Identity
	Directory        string
	settingsPath     string
	daily            bool
	WorkspaceRoot    string
	Cwd              string
	Token            string
	Cancel           context.CancelFunc
	connections      sync.Map
	clients          chan struct{}
	workers          chan struct{}
	authWorkers      chan struct{}
	mcpAuth          *backendmcpauth.Service
	revision         atomic.Uint64
	fileMu           sync.Mutex
	taskEditMu       sync.RWMutex
	managementMu     sync.RWMutex
	lifetime         context.Context
	usageMu          sync.Mutex
	usage            *backendusage.Service
	usageClosed      bool
	draftGeneration  int64
	events           *backendevents.Hub
	runtime          *backendruntime.Manager
	attachments      *backendattachments.Service
	services         *backendservices.Service
	servicePool      *servicePool
	skills           *backendskills.Service
	quota            *backendquota.Service
	queries          backendworkspace.Queries
	queryAdmissionMu sync.Mutex
	queryClosed      bool
	queryWork        sync.WaitGroup
	loadLogged       sync.Once
	queryLifetime    context.Context
	queryCancel      context.CancelFunc
	scheduledCancel  context.CancelFunc
	scheduledDone    chan struct{}
	scheduledWake    chan struct{}
}

func New(ctx context.Context, store *backendstore.Store, identity Identity, directory, settingsPath, cwd, token string, daily bool, cancel context.CancelFunc) *Server {
	s := &Server{Store: store, Identity: identity, Directory: directory, settingsPath: settingsPath, WorkspaceRoot: filepath.Join(filepath.Dir(directory), "workspace"), Cwd: cwd, Token: token, Cancel: cancel, clients: make(chan struct{}, 32), workers: make(chan struct{}, 8), events: backendevents.New(identity.InstanceID, backendevents.Options{})}
	s.daily = daily
	profileRoot := filepath.Dir(directory)
	if daily {
		profileRoot = directory
		s.WorkspaceRoot = directory
	}
	s.lifetime = ctx
	s.queryLifetime, s.queryCancel = context.WithCancel(ctx)
	s.authWorkers = make(chan struct{}, 16)
	home := filepath.Join(profileRoot, "home")
	if daily {
		home, _ = os.UserHomeDir()
	}
	s.mcpAuth = backendmcpauth.New(ctx, profileRoot, daily, nil)
	s.runtime = backendruntime.New(ctx, profileRoot, settingsPath, daily, store, func(session, runtime, kind string, payload any) error {
		raw, err := json.Marshal(payload)
		if err != nil {
			return err
		}
		return s.events.Emit(session, runtime, kind, raw, true)
	})
	s.runtime.EventCursor = func(session, runtime string) (model.RuntimeEventCursor, bool) {
		cursor, ok := s.events.Cursor(session, runtime)
		return model.RuntimeEventCursor{RuntimeID: cursor.RuntimeID, Epoch: cursor.Epoch, Sequence: cursor.Sequence}, ok
	}
	s.runtime.OnStart = func(session, runtime string) { s.events.Activate(session, runtime) }
	s.runtime.OnStop = func(session, runtime string) { s.events.Deactivate(session, runtime) }
	s.attachments = backendattachments.New(directory, s.WorkspaceRoot, daily, func(ctx context.Context) (map[string]bool, error) { return store.AssetReferences(ctx, directory) })
	s.servicePool = &servicePool{root: profileRoot, settingsPath: settingsPath, daily: daily, processes: map[string]*backendengine.Process{}}
	s.services = backendservices.New(filepath.Dir(directory), s.servicePool.acquire)
	s.skills = backendskills.New(profileRoot, home, daily)
	localQuota := backendquota.NewLocal(home, nil)
	s.quota = backendquota.New(nil, localQuota.Fetch)
	scheduledCtx, scheduledCancel := context.WithCancel(ctx)
	s.scheduledCancel = scheduledCancel
	s.scheduledDone = make(chan struct{})
	s.scheduledWake = make(chan struct{}, 1)
	go s.runScheduled(scheduledCtx)
	return s
}

type peer struct {
	conn   *websocket.Conn
	out    chan []byte
	done   chan struct{}
	once   sync.Once
	queued atomic.Int64
}

func (p *peer) close() { p.once.Do(func() { close(p.done); _ = p.conn.Close() }) }

func (p *peer) send(value any) bool {
	raw, err := json.Marshal(value)
	if err != nil {
		log.Printf("encode daemon response: %v", err)
		p.close()
		return false
	}
	if len(raw) > maxFrame {
		log.Printf("daemon response exceeds wire limit: %d bytes", len(raw))
		p.close()
		return false
	}
	if p.queued.Add(int64(len(raw))) > maxFrame {
		p.queued.Add(-int64(len(raw)))
		p.close()
		return false
	}
	select {
	case p.out <- raw:
		return true
	default:
		p.queued.Add(-int64(len(raw)))
		p.close()
		return false
	}
}

func (p *peer) write() {
	defer p.close()
	for {
		select {
		case <-p.done:
			return
		case raw := <-p.out:
			p.queued.Add(-int64(len(raw)))
			_ = p.conn.SetWriteDeadline(time.Now().Add(15 * time.Second))
			if err := p.conn.WriteMessage(websocket.TextMessage, raw); err != nil {
				return
			}
		}
	}
}

func (s *Server) Close() {
	s.queryAdmissionMu.Lock()
	s.queryClosed = true
	s.queryCancel()
	s.queryAdmissionMu.Unlock()
	s.queryWork.Wait()
	s.mcpAuth.Close()
	s.scheduledCancel()
	<-s.scheduledDone
	s.usageMu.Lock()
	s.usageClosed = true
	usage := s.usage
	s.usageMu.Unlock()
	if usage != nil {
		_ = usage.Close()
	}
	s.servicePool.close()
	s.runtime.Close()
	s.events.Close()
	s.connections.Range(func(key, value any) bool { key.(*peer).close(); return true })
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/v1" || r.Method != http.MethodGet {
		http.NotFound(w, r)
		return
	}
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil || !net.ParseIP(host).IsLoopback() {
		http.Error(w, "loopback only", http.StatusForbidden)
		return
	}
	select {
	case s.clients <- struct{}{}:
		defer func() { <-s.clients }()
	default:
		http.Error(w, "connection limit", http.StatusServiceUnavailable)
		return
	}
	upgrader := websocket.Upgrader{ReadBufferSize: 4096, WriteBufferSize: 4096, HandshakeTimeout: 5 * time.Second, CheckOrigin: func(r *http.Request) bool { return r.Header.Get("Origin") == "" }}
	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	p := &peer{conn: conn, out: make(chan []byte, 32), done: make(chan struct{})}
	defer p.close()
	queryCtx, cancelQueries := context.WithCancel(r.Context())
	stopQueries := context.AfterFunc(s.queryLifetime, cancelQueries)
	defer stopQueries()
	var queryWorkers sync.WaitGroup
	defer func() { p.close(); cancelQueries(); queryWorkers.Wait() }()
	conn.SetReadLimit(maxFrame)
	_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
	var hello struct {
		Type     string                       `json:"type"`
		Version  int                          `json:"protocolVersion"`
		Token    string                       `json:"token"`
		ClientID string                       `json:"clientId"`
		Resume   []backendevents.ReplayCursor `json:"resumeFrom"`
	}
	if err := conn.ReadJSON(&hello); err != nil {
		return
	}
	if hello.Type != "hello" || hello.Version != daemon.ProtocolVersion || subtle.ConstantTimeCompare([]byte(hello.Token), []byte(s.Token)) != 1 || uuid.Validate(hello.ClientID) != nil {
		_ = conn.SetWriteDeadline(time.Now().Add(5 * time.Second))
		_ = conn.WriteJSON(map[string]any{"type": "rejected", "message": "authentication failed"})
		return
	}
	_ = conn.SetReadDeadline(time.Time{})
	go p.write()
	s.connections.Store(p, true)
	defer s.connections.Delete(p)
	if !p.send(map[string]any{"type": "hello", "protocolVersion": daemon.ProtocolVersion, "daemonVersion": Version, "environmentId": s.Identity.EnvironmentID, "instanceId": s.Identity.InstanceID}) {
		return
	}
	subscription, err := s.events.Subscribe(hello.Resume)
	if err != nil {
		p.send(map[string]any{"type": "rejected", "message": "event replay unavailable; reload saved sessions"})
		return
	}
	defer subscription.Close()
	go func() {
		for {
			select {
			case <-p.done:
				return
			case frame, ok := <-subscription.Events:
				if !ok {
					p.close()
					return
				}
				if !p.send(json.RawMessage(frame)) {
					return
				}
			}
		}
	}()
	for {
		var envelope struct {
			Type      string          `json:"type"`
			RequestID string          `json:"requestId"`
			SessionID string          `json:"sessionId"`
			RuntimeID string          `json:"runtimeId"`
			Command   json.RawMessage `json:"command"`
		}
		if err := conn.ReadJSON(&envelope); err != nil {
			return
		}
		if envelope.Type == "shutdown" {
			s.Cancel()
			return
		}
		if envelope.Type != "request" || uuid.Validate(envelope.RequestID) != nil || uuid.Validate(envelope.SessionID) != nil || uuid.Validate(envelope.RuntimeID) != nil {
			return
		}
		command, decodeErr := backendstore.Decode(envelope.Command)
		if decodeErr == nil {
			operation, handled, err := cancellableWorkspaceQuery(command)
			if err != nil {
				if !sendQueryResponse(p, envelope.RequestID, nil, err) {
					return
				}
				continue
			}
			if handled {
				if operation.String("type") == "cancelGitQuery" {
					err := s.queries.Cancel(operation.String("queryId"))
					result := backendstore.Reply("workspace", backendstore.Fields(map[string]any{"result": map[string]string{"type": "ack"}}))
					if !sendQueryResponse(p, envelope.RequestID, result, err) {
						return
					}
					continue
				}
				if s.admitWorkspaceQuery() {
					request := envelope
					queryWorkers.Add(1)
					go func() {
						defer s.queryWork.Done()
						defer queryWorkers.Done()
						defer func() { <-s.workers }()
						ctx, cancel := context.WithTimeout(queryCtx, 30*time.Second)
						defer cancel()
						value, err := s.dispatch(ctx, request.SessionID, request.RuntimeID, request.Command)
						sendQueryResponse(p, request.RequestID, value, err)
					}()
				} else {
					if !sendQueryResponse(p, envelope.RequestID, nil, errors.New("daemon request capacity reached")) {
						return
					}
				}
				continue
			}
		}
		if decodeErr == nil && command.String("type") == "authenticateMcpServer" {
			select {
			case s.authWorkers <- struct{}{}:
				request := envelope
				go func() {
					defer func() { <-s.authWorkers }()
					ctx, cancel := context.WithTimeout(r.Context(), 400*time.Second)
					defer cancel()
					value, err := s.dispatch(ctx, request.SessionID, request.RuntimeID, request.Command)
					outcome := map[string]any{"status": "ok", "payload": value}
					if err != nil {
						outcome = map[string]any{"status": "error", "error": map[string]string{"message": err.Error()}}
					}
					p.send(map[string]any{"type": "response", "requestId": request.RequestID, "outcome": outcome})
				}()
			default:
				p.send(map[string]any{"type": "response", "requestId": envelope.RequestID, "outcome": map[string]any{"status": "error", "error": map[string]string{"message": "MCP authorization capacity reached"}}})
			}
			continue
		}
		if decodeErr == nil && command.String("type") == "generateSessionText" {
			// Keep reading this connection so its cancellation can overtake the
			// HTTP exchange. Admission happens before spawning, never in an
			// unbounded queue of waiting goroutines.
			select {
			case s.workers <- struct{}{}:
				request := envelope
				go func() {
					defer func() { <-s.workers }()
					ctx, cancel := context.WithTimeout(r.Context(), 120*time.Second)
					defer cancel()
					result, err := s.dispatch(ctx, request.SessionID, request.RuntimeID, request.Command)
					outcome := map[string]any{"status": "ok", "payload": result}
					if err != nil {
						outcome = map[string]any{"status": "error", "error": map[string]string{"message": err.Error()}}
					}
					p.send(map[string]any{"type": "response", "requestId": request.RequestID, "outcome": outcome})
				}()
			default:
				if !p.send(map[string]any{"type": "response", "requestId": envelope.RequestID, "outcome": map[string]any{"status": "error", "error": map[string]string{"message": "daemon request capacity reached"}}}) {
					return
				}
			}
			continue
		}
		budget := 30 * time.Second
		if decodeErr == nil && command.String("type") == "workspace" {
			operation, _ := backendstore.Decode(command["operation"])
			switch operation.String("type") {
			case "generateCommitMessage":
				budget = 180 * time.Second
			case "discoverSlashCommands":
				budget = 65 * time.Second
			case "fetchRemote", "pullFastForward", "push", "commit":
				budget = 120 * time.Second
			}
		}
		if decodeErr == nil && command.String("type") == "fetchProviderQuota" && command.String("providerId") == "anthropic" {
			budget = 80 * time.Second
		}
		if decodeErr == nil && (command.String("type") == "installOpenCode" || command.String("type") == "updateOpenCode") {
			budget = 300 * time.Second
		}
		ctx, cancel := context.WithTimeout(r.Context(), budget)
		var result backendstore.Object
		var callErr error
		if decodeErr == nil && (command.String("type") == "cancelSessionGeneration" || command.String("type") == "cancelAuthenticateMcpServer") {
			// Cancellation does no I/O and must remain available when every
			// ordinary worker is waiting for a generation answer.
			result, callErr = s.dispatch(ctx, envelope.SessionID, envelope.RuntimeID, envelope.Command)
		} else {
			select {
			case s.workers <- struct{}{}:
				result, callErr = s.dispatch(ctx, envelope.SessionID, envelope.RuntimeID, envelope.Command)
				<-s.workers
			case <-ctx.Done():
				callErr = ctx.Err()
			default:
				callErr = errors.New("daemon request capacity reached")
			}
		}
		cancel()
		outcome := map[string]any{"status": "ok", "payload": result}
		if callErr != nil {
			outcome = map[string]any{"status": "error", "error": map[string]string{"message": callErr.Error()}}
		}
		if !p.send(map[string]any{"type": "response", "requestId": envelope.RequestID, "outcome": outcome}) {
			return
		}
	}
}

func (s *Server) dispatch(ctx context.Context, sessionID, runtimeID string, raw json.RawMessage) (backendstore.Object, error) {
	command, err := backendstore.Decode(raw)
	if err != nil {
		return nil, err
	}
	if kind := command.String("type"); kind == "installOpenCode" || kind == "updateOpenCode" {
		if !s.managementMu.TryLock() {
			return nil, errors.New("OpenCode management is busy; finish active requests first")
		}
		defer s.managementMu.Unlock()
		return s.manageOpenCode(ctx, command)
	}
	if !s.managementMu.TryRLock() {
		return nil, errors.New("OpenCode management is in progress")
	}
	defer s.managementMu.RUnlock()
	if kind := command.String("type"); kind == "authenticateMcpServer" || kind == "cancelAuthenticateMcpServer" {
		return s.mcpAuthCommand(ctx, command)
	}
	if kind := command.String("type"); kind == "forkSessionFromResponse" || kind == "rewindSessionToMessage" {
		s.taskEditMu.Lock()
		defer s.taskEditMu.Unlock()
		return s.taskEdit(ctx, sessionID, command)
	}
	if kind := command.String("type"); kind == "previewSessionCleanup" || kind == "cleanupOldSession" {
		s.taskEditMu.Lock()
		defer s.taskEditMu.Unlock()
		return s.cleanupCommand(ctx, sessionID, command)
	}
	switch command.String("type") {
	case "fork", "start", "dispatchSubmission", "abandonSubmission", "cancel", "respond", "compactSession", "steer", "respondUserInput", "cancelUserInput", "applyOptions", "goal", "stopBackgroundWork", "compactChildSession", "refreshBackgroundWork", "closeSession", "acceptSubmission", "saveTaskState", "saveTaskStateCheckpoint", "removeSession", "removeProject", "forkProviderSession", "saveComposerDrafts", "applyComposerDraftChanges":
		s.taskEditMu.RLock()
		defer s.taskEditMu.RUnlock()
	}
	if response, handled, err := s.attachments.Handle(ctx, raw); handled {
		if err != nil {
			return nil, err
		}
		return backendstore.Decode(response)
	}
	if command.String("type") == "probeProvider" {
		return s.probeProvider(ctx, command)
	}
	if response, handled, err := s.services.Handle(ctx, raw); handled {
		if err != nil {
			return nil, err
		}
		return backendstore.Decode(response)
	}
	if response, handled, err := s.skills.Handle(ctx, command); handled {
		return response, err
	}
	switch command.String("type") {
	case "openTerminal":
		return nil, errors.New("daemon terminals are not supported on this platform")
	case "writeTerminal", "resizeTerminal":
		return nil, errors.New("daemon terminal " + sessionID + " is not running")
	case "closeTerminal":
		// No terminal can be opened on this platform; closing an absent one
		// remains idempotent, matching the Rust daemon.
		return backendstore.Reply("ack", nil), nil
	case "fetchUsageStats":
		return s.usageStats(ctx, command)
	case "checkOpenCodeUpdate":
		return s.checkOpenCodeUpdate(ctx)
	case "fetchPlanUsage":
		home := filepath.Join(filepath.Dir(s.Directory), "home")
		if s.daily {
			home, _ = os.UserHomeDir()
		}
		usage, err := s.quota.PlanUsage(ctx, home)
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("planUsage", backendstore.Fields(map[string]any{"usage": usage})), nil
	case "fetchProviderQuota":
		provider := command.String("providerId")
		var api backendquota.API
		if backendquota.Supported(provider) && provider != "anthropic" && provider != "openai" {
			service, err := s.servicePool.acquire(ctx, command)
			if err != nil {
				return backendstore.Reply("providerQuota", backendstore.Fields(map[string]any{"quota": s.quota.Fetch(ctx, "", nil, provider, command.Bool("force"))})), nil
			}
			api = service
		}
		return backendstore.Reply("providerQuota", backendstore.Fields(map[string]any{"quota": s.quota.Fetch(ctx, command.String("binary")+"\x00"+command.String("directory"), api, provider, command.Bool("force"))})), nil
	case "listScheduledTasks", "saveScheduledTask", "deleteScheduledTask", "runScheduledTaskNow":
		return s.scheduledCommand(ctx, command)
	case "fork", "start", "attachSession", "dispatchSubmission", "abandonSubmission", "cancel", "respond", "compactSession", "steer", "respondUserInput", "cancelUserInput", "applyOptions", "goal", "stopBackgroundWork", "compactChildSession", "refreshBackgroundWork":
		if sessionID == uuid.Nil.String() {
			return nil, errors.New("session ID required")
		}
		result, err := s.runtime.Handle(ctx, sessionID, runtimeID, command)
		return result, err
	case "closeSession":
		if sessionID == uuid.Nil.String() {
			return nil, errors.New("session ID required")
		}
		result, err := s.runtime.Handle(ctx, sessionID, runtimeID, command)
		if err != nil {
			return nil, err
		}
		s.runtime.Stop(sessionID)
		return result, nil
	case "acceptSubmission":
		receipt, err := s.Store.Admit(ctx, sessionID, command)
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("submissionSaved", backendstore.Fields(map[string]any{"receipt": receipt})), nil
	case "workspace":
		if response, handled, err := s.workspaceService(ctx, command["operation"]); handled {
			return response, err
		}
		if response, handled, err := s.projectlessOperation(ctx, command["operation"]); handled {
			return response, err
		}
		if response, handled, err := s.projectOperation(ctx, command["operation"]); handled {
			return response, err
		}
		root, operation, err := s.workspaceOperation(command["operation"])
		if err != nil {
			return nil, err
		}
		raw, err := backendworkspace.DispatchWithQueries(ctx, root, operation, &s.queries)
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("workspace", backendstore.Fields(map[string]any{"result": json.RawMessage(raw)})), nil
	case "loadTaskState":
		result, err := s.Store.Load(ctx, s.Cwd)
		if err != nil {
			return nil, err
		}
		general, err := s.generalWorkspace(true)
		if err != nil {
			return nil, err
		}
		result.Set("projectlessRoot", general)
		legacy := filepath.Join(filepath.Dir(s.Directory), "home", ".fintwind")
		if s.daily {
			if home, e := os.UserHomeDir(); e == nil {
				legacy = filepath.Join(home, ".fintwind")
			}
		}
		result.Set("projectlessLegacyRoot", legacy)
		if s.daily {
			s.loadLogged.Do(func() {
				var projects, sessions []json.RawMessage
				if json.Unmarshal(result["projects"], &projects) == nil && json.Unmarshal(result["sessions"], &sessions) == nil {
					log.Printf("loaded task catalog: %d projects, %d sessions", len(projects), len(sessions))
				}
			})
		}
		return result, nil
	case "hydrateSession":
		id, err := command.ID("sessionId")
		if err != nil {
			return nil, err
		}
		return s.Store.Hydrate(ctx, id)
	case "loadMcpServers", "saveMcpServers":
		return s.mcpServers(command)
	case "saveTaskState", "saveTaskStateCheckpoint":
		result, err := s.Store.Save(ctx, command)
		if err == nil {
			s.notify("taskStateChanged")
		}
		return result, err
	case "removeSession":
		if sessionID == uuid.Nil.String() {
			return nil, errors.New("session ID required")
		}
		if err := s.Store.Remove(ctx, sessionID, ""); err != nil {
			return nil, err
		}
		s.runtime.Stop(sessionID)
		s.notify("taskStateChanged")
		return backendstore.Reply("ack", nil), nil
	case "removeProject":
		id, err := command.ID("projectId")
		if err != nil {
			return nil, err
		}
		sessions, err := s.Store.SessionsForProject(ctx, id)
		if err != nil {
			return nil, err
		}
		if err := s.Store.Remove(ctx, "", id); err != nil {
			return nil, err
		}
		for _, doomed := range sessions {
			s.runtime.Stop(doomed)
		}
		s.notify("taskStateChanged")
		return backendstore.Reply("ack", nil), nil
	case "searchSessionMessages":
		limit, err := command.Int("limit")
		if err != nil {
			return nil, err
		}
		return s.Store.Search(ctx, command.String("query"), int(limit))
	case "getSettings", "updateSettings", "updateSetting", "loadComposerDrafts", "saveComposerDrafts", "applyComposerDraftChanges":
		return s.files(command)
	default:
		return nil, errors.New("unsupported command in Go backend")
	}
}

func (s *Server) notify(kind string) {
	value := map[string]any{"type": kind, "revision": s.revision.Add(1)}
	s.connections.Range(func(key, valueIgnored any) bool { key.(*peer).send(value); return true })
}
