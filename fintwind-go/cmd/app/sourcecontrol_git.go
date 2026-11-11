package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/model"
	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/editor"
)

const (
	scSecondaryWidth  = 260.0
	scRowHeight       = 28.0
	scRefreshInterval = 5 * time.Second
	scGitTimeout      = 25 * time.Second
)

type scScope struct {
	Client                            *daemon.Client
	Identity                          daemon.Identity
	ProjectID, ProjectRoot, SessionID string
	SessionGeneration                 int
}

type scFile struct {
	Path, Origin, Group, Letter string
	Additions, Deletions        uint64
	Binary                      bool
}

func (f scFile) key() string { return f.Group + "\x00" + f.Path }

type scWriteKey struct{ Environment, Root string }
type scWriteState struct {
	Busy    bool
	Unknown bool
	Label   string
}

type sourceControlState struct {
	Scope                           scScope
	Root, Branch, Error, Notice     string
	Ahead, Behind                   uint64
	Generation                      uint64
	Loading, Loaded, IsRepo         bool
	LastRefresh                     time.Time
	Files, Rows                     []scFile
	ListRows                        []scListRow
	FilesByKey                      map[string]scFile
	HasStaged                       bool
	Selected, Filter, AppliedFilter string
	FileList                        ui.ListState
	FileIndex                       int
	Reader                          reviewReader
	LayoutSel                       int // 0 unified, 1 split; the reader follows it
	CommitOpen, IncludeAll          bool
	CommitMessage                   string
	CommitPush                      bool
	CommitMoreOpen, RepoMenuOpen    bool
	Discard                         *scFile
	BranchOpen, BranchLoading       bool
	Branches                        []scBranch
	BranchRows                      []scBranch
	BranchList                      ui.ListState
	BranchFilter                    string
	BranchCreate                    string
	BranchGeneration                uint64
	Writes                          map[scWriteKey]*scWriteState
	Source                          reviewSource
	SourceMenuOpen                  bool
	Stats                           reviewAreaStats
	StatsGen                        uint64
	StatsLoading                    bool
	// Directory folding mirrors Rust's SOURCE_CONTROL_TREE_INDENT tree:
	// collapsed dirs hide their files, group headers carry counts.
	CollapsedDirs map[string]bool
	// Ignored section mirrors Rust's toggleable ignored block.
	IgnoredOpen    bool
	IgnoredLoading bool
	Ignored        []scFile
	IgnoredError   string
}

// reviewSource mirrors ReviewDiffSource in
// crates/fintwind-protocol/src/workspace.rs: the live area the toolbar shows
// plus the checkpoint source used for turn review.
type reviewSource string

const (
	reviewSourceUncommitted reviewSource = "uncommitted"
	reviewSourceUnstaged    reviewSource = "unstaged"
	reviewSourceStaged      reviewSource = "staged"
	reviewSourceCommitted   reviewSource = "committed"
	reviewSourceBranch      reviewSource = "branch"
)

func (s reviewSource) wire() any {
	return string(s)
}

func (s reviewSource) label() string {
	switch s {
	case reviewSourceUncommitted:
		return tr("Uncommitted")
	case reviewSourceUnstaged:
		return tr("Unstaged")
	case reviewSourceStaged:
		return tr("Staged")
	case reviewSourceCommitted:
		return tr("Committed")
	case reviewSourceBranch:
		return tr("Branch")
	default:
		return tr("Uncommitted")
	}
}

type reviewAreaStats struct {
	Additions, Deletions, Files uint64
	Truncated                   bool
	Valid                       bool
}

type scBranch struct {
	Name      string `json:"name"`
	Elsewhere bool   `json:"checked_out_elsewhere"`
}

type scWireSession struct {
	ID        string `json:"id"`
	ProjectID string `json:"project_id"`
	Workspace struct {
		Kind string `json:"kind"`
		Path string `json:"path"`
	} `json:"workspace"`
	Turns []struct {
		ID         string `json:"id"`
		TurnCount  int    `json:"turn_count"`
		Checkpoint *struct {
			Status    string                 `json:"status"`
			GitRef    string                 `json:"git_ref"`
			TurnCount int                    `json:"turn_count"`
			Files     []model.CheckpointFile `json:"files"`
		} `json:"checkpoint"`
	} `json:"turns"`
}

func (a *app) scScope() scScope {
	p := a.selectedProject()
	if p == nil || a.client == nil {
		return scScope{}
	}
	identity := a.client.Identity()
	if p.EnvironmentID != "" && p.EnvironmentID != "00000000-0000-0000-0000-000000000000" && p.EnvironmentID != identity.EnvironmentID {
		return scScope{}
	}
	return scScope{Client: a.client, Identity: identity, ProjectID: p.ID, ProjectRoot: p.Path, SessionID: a.selectedID, SessionGeneration: a.generation}
}

func (a *app) scCurrent(scope scScope) bool { return scope.Client != nil && a.scScope() == scope }

func scRequest(ctx context.Context, client *daemon.Client, operation map[string]any, result any) error {
	if client == nil {
		return errors.New("Daemon unavailable")
	}
	payload, err := client.Request(ctx, map[string]any{"type": "workspace", "operation": operation})
	if err != nil {
		return err
	}
	var envelope struct {
		Type   string          `json:"type"`
		Result json.RawMessage `json:"result"`
	}
	if err = json.Unmarshal(payload, &envelope); err != nil {
		return err
	}
	if envelope.Type != "workspace" || len(envelope.Result) == 0 {
		return errors.New("Invalid workspace response")
	}
	return json.Unmarshal(envelope.Result, result)
}

func scHydrate(ctx context.Context, scope scScope) (*scWireSession, string, error) {
	if scope.SessionID == "" {
		if scope.ProjectRoot == "" || !filepath.IsAbs(scope.ProjectRoot) {
			return nil, "", errors.New("Workspace path is unavailable")
		}
		return nil, scope.ProjectRoot, nil
	}
	payload, err := scope.Client.Request(ctx, map[string]any{
		"type": "hydrateSession", "sessionId": scope.SessionID, "environmentId": "00000000-0000-0000-0000-000000000000",
	})
	if err != nil {
		return nil, "", err
	}
	var envelope struct {
		Session *scWireSession `json:"session"`
	}
	if err = json.Unmarshal(payload, &envelope); err != nil {
		return nil, "", err
	}
	s := envelope.Session
	if s == nil || s.ID != scope.SessionID || s.ProjectID != scope.ProjectID {
		return nil, "", errors.New("Session workspace identity changed")
	}
	root := scope.ProjectRoot
	switch s.Workspace.Kind {
	case "", "local":
	case "newWorktree":
		return nil, "", errors.New("The session worktree has not been created yet")
	case "worktree":
		root = s.Workspace.Path
	default:
		return nil, "", errors.New("Unsupported session workspace")
	}
	if root == "" || !filepath.IsAbs(root) {
		return nil, "", errors.New("Workspace path is unavailable")
	}
	return s, root, nil
}

func scValidPath(path string) bool {
	if path == "" || strings.ContainsAny(path, "\x00\\") || strings.HasPrefix(path, "/") || strings.Contains(path, ":") {
		return false
	}
	for _, part := range strings.Split(path, "/") {
		if part == ".." || part == "." || part == "" {
			return false
		}
	}
	return true
}

func (a *app) scEnsureFresh() {
	st := &a.sourceControl
	scope := a.scScope()
	if st.Scope != scope {
		*st = sourceControlState{Scope: scope, Generation: st.Generation + 1, BranchGeneration: st.BranchGeneration + 1, Writes: st.Writes, CommitMessage: st.CommitMessage, Source: reviewSourceUncommitted, CollapsedDirs: map[string]bool{}}
	}
	if st.Source == "" {
		st.Source = reviewSourceUncommitted
	}
	if st.CollapsedDirs == nil {
		st.CollapsedDirs = map[string]bool{}
	}
	if scope.Client != nil && !st.Loading && time.Since(st.LastRefresh) >= scRefreshInterval {
		a.scRefresh(false)
	}
}

func (a *app) scRefresh(manual bool) {
	st := &a.sourceControl
	if !a.scCurrent(st.Scope) || st.Loading {
		return
	}
	scope := st.Scope
	st.Generation++
	gen := st.Generation
	loaded := st.Loaded
	st.Loading = true
	st.LastRefresh = time.Now()
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		_, root, err := scHydrate(ctx, scope)
		var response struct {
			Type   string `json:"type"`
			Status *struct {
				Branch  *string `json:"branch"`
				Ahead   uint64  `json:"ahead"`
				Behind  uint64  `json:"behind"`
				Entries []struct {
					Path     string  `json:"path"`
					Origin   *string `json:"origin_path"`
					Index    string  `json:"index_status"`
					Worktree string  `json:"worktree_status"`
				} `json:"entries"`
			} `json:"status"`
		}
		if err == nil {
			err = scRequest(ctx, scope.Client, map[string]any{"type": "inspectStatus", "cwd": root}, &response)
		}
		if err == nil && response.Type != "worktreeStatus" {
			err = errors.New("Invalid Git status response")
		}
		var files []scFile
		branch := "Detached HEAD"
		if err == nil && response.Status != nil {
			if response.Status.Branch != nil {
				branch = *response.Status.Branch
			}
			for _, entry := range response.Status.Entries {
				if !scValidPath(entry.Path) {
					err = errors.New("Git reported an unsupported path")
					break
				}
				origin := ""
				if entry.Origin != nil {
					origin = *entry.Origin
					if !scValidPath(origin) {
						err = errors.New("Git reported an unsupported rename path")
						break
					}
				}
				f := scFile{Path: entry.Path, Origin: origin}
				if entry.Index == "?" || entry.Worktree == "?" {
					f.Group, f.Letter = "untracked", "?"
					files = append(files, f)
					continue
				}
				if entry.Index != " " && entry.Index != "!" && entry.Index != "" {
					f.Group, f.Letter = "staged", entry.Index
					files = append(files, f)
				}
				if entry.Worktree != " " && entry.Worktree != "!" && entry.Worktree != "" {
					f.Group, f.Letter = "changes", entry.Worktree
					if entry.Worktree != "R" && entry.Worktree != "C" {
						f.Origin = ""
					}
					files = append(files, f)
				}
			}
		}
		a.post(func() {
			st := &a.sourceControl
			if !a.scCurrent(scope) || st.Scope != scope || st.Generation != gen {
				return
			}
			st.Loading, st.Loaded = false, true
			if err != nil {
				// A background refresh failure must not blank a filled
				// surface; only a manual refresh or the first load reports
				// the error.
				if manual || !loaded {
					st.Error = err.Error()
				}
				return
			}
			rootChanged := st.Root != root
			if rootChanged {
				st.Discard = nil
				st.BranchOpen = false
				st.Selected = ""
				st.BranchGeneration++
				st.BranchLoading = false
				st.Branches, st.BranchRows = nil, nil
				st.Stats = reviewAreaStats{}
			}
			st.Root, st.Branch, st.Error, st.IsRepo = root, branch, "", response.Status != nil
			if response.Status != nil {
				st.Ahead, st.Behind = response.Status.Ahead, response.Status.Behind
			} else {
				st.Ahead, st.Behind = 0, 0
			}
			st.Files = files
			if manual {
				if write := st.writeState(); write != nil && write.Unknown && !write.Busy {
					write.Unknown = false
					st.Notice = "Status refreshed. Check Git history before submitting the commit again."
				}
			}
			st.FilesByKey = make(map[string]scFile, len(files))
			st.HasStaged = false
			for _, file := range files {
				st.FilesByKey[file.key()] = file
				if file.Group == "staged" {
					st.HasStaged = true
				}
			}
			st.rebuildRows()
			a.scLoadDiff()
			a.scLoadStats()
		})
	}()
}

// scSetSource retargets the review source like Rust's
// set_review_diff_source. The live sources reload the shown file and area
// totals; the committed and branch ranges are whole-source snapshots, which
// the review surface loads once and reads without touching Git again.
func (a *app) scSetSource(source reviewSource) {
	st := &a.sourceControl
	if source == reviewSourceCommitted || source == reviewSourceBranch {
		st.SourceMenuOpen = false
		a.openSourceReview(source)
		return
	}
	if st.Source == source {
		st.SourceMenuOpen = false
		return
	}
	st.Source = source
	st.SourceMenuOpen = false
	st.Reader.Context = 3
	// The previous source's totals must not show beside the new one while
	// its own are in flight.
	st.Stats = reviewAreaStats{}
	a.scLoadDiff()
	a.scLoadStats()
}

// scLoadStats fetches the toolbar capsule totals for the current source,
// mirroring refresh_review_stats: one figure for the whole area beside the
// source pill.
func (a *app) scLoadStats() {
	st := &a.sourceControl
	if !a.scCurrent(st.Scope) || st.Root == "" {
		return
	}
	scope, root, source := st.Scope, st.Root, st.Source
	st.StatsGen++
	gen := st.StatsGen
	st.StatsLoading = true
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		var response struct {
			Type      string `json:"type"`
			Additions uint64 `json:"additions"`
			Deletions uint64 `json:"deletions"`
			Files     uint64 `json:"files"`
			Truncated bool   `json:"truncated"`
		}
		var sourceWire any = string(source)
		err := scRequest(ctx, scope.Client, map[string]any{"type": "collectReviewStats", "cwd": root, "source": sourceWire}, &response)
		a.post(func() {
			st := &a.sourceControl
			if !a.scCurrent(scope) || st.Scope != scope || st.Root != root || st.StatsGen != gen || st.Source != source {
				return
			}
			st.StatsLoading = false
			if err != nil || response.Type != "reviewStats" {
				// Keep the totals already on screen; a transient failure
				// must not blank the pill every refresh.
				return
			}
			st.Stats = reviewAreaStats{Additions: response.Additions, Deletions: response.Deletions, Files: response.Files, Truncated: response.Truncated, Valid: response.Additions > 0 || response.Deletions > 0}
		})
	}()
}

// scRefreshSurface reloads the shown file and the area totals so the
// toolbar number never lags the reader, mirroring refresh_review_surface.
// scRefresh's callback reloads both, so one call suffices.
func (a *app) scRefreshSurface() {
	a.scRefresh(true)
}

// scGrowContext requests 100 more context lines around the live file's
// hunks, the re-request half of Rust's gap expansion.
func (a *app) scGrowContext() {
	st := &a.sourceControl
	next := min(100000, st.Reader.Context+100)
	if next <= st.Reader.Context {
		next = min(100000, max(20, st.Reader.Context*5))
	}
	st.Reader.Context = next
	a.scLoadDiff()
}

// scStepFile moves the reader to the previous/next changed file, mirroring
// step_source_control_file: the list resets to the top on switch.
func (a *app) scStepFile(delta int) {
	st := &a.sourceControl
	if len(st.Rows) == 0 {
		return
	}
	next := st.FileIndex + delta
	if next < 0 {
		next = 0
	}
	if next >= len(st.Rows) {
		next = len(st.Rows) - 1
	}
	if next == st.FileIndex {
		return
	}
	st.FileIndex = next
	file := st.Rows[next]
	st.Selected = file.key()
	st.Reader.Context = 3
	a.scLoadDiff()
}

func (a *app) scLoadIgnored() {
	st := &a.sourceControl
	if !a.scCurrent(st.Scope) || st.Root == "" || st.IgnoredLoading {
		return
	}
	scope, root := st.Scope, st.Root
	st.IgnoredLoading = true
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		var response struct {
			Type    string `json:"type"`
			Entries []struct {
				Path string `json:"path"`
			} `json:"entries"`
		}
		err := scRequest(ctx, scope.Client, map[string]any{"type": "listIgnored", "cwd": root}, &response)
		a.post(func() {
			st := &a.sourceControl
			if !a.scCurrent(scope) || st.Scope != scope || st.Root != root {
				return
			}
			st.IgnoredLoading = false
			if err != nil {
				st.IgnoredError = err.Error()
				return
			}
			st.IgnoredError = ""
			files := make([]scFile, 0, len(response.Entries))
			for _, entry := range response.Entries {
				if !scValidPath(entry.Path) {
					continue
				}
				files = append(files, scFile{Path: entry.Path, Group: "ignored", Letter: "!"})
			}
			st.Ignored = files
		})
	}()
}

func (st *sourceControlState) rebuildRows() {
	st.AppliedFilter = st.Filter
	st.Rows = st.Rows[:0]
	for _, group := range []string{"staged", "changes", "untracked"} {
		for _, f := range st.Files {
			if f.Group == group && strings.Contains(strings.ToLower(f.Path), strings.ToLower(st.Filter)) {
				st.Rows = append(st.Rows, f)
			}
		}
	}
	if st.selectedFile() == nil {
		st.Selected = ""
		if len(st.Rows) > 0 {
			st.Selected = st.Rows[0].key()
		}
	}
	st.FileIndex = -1
	for i, row := range st.Rows {
		if row.key() == st.Selected {
			st.FileIndex = i
			break
		}
	}
	st.buildListRows()
}

func (st *sourceControlState) selectedFile() *scFile {
	if file, ok := st.FilesByKey[st.Selected]; ok {
		return &file
	}
	return nil
}

func (st *sourceControlState) writeKey() scWriteKey {
	return scWriteKey{st.Scope.Identity.EnvironmentID, st.Root}
}
func (st *sourceControlState) writeState() *scWriteState { return st.Writes[st.writeKey()] }
func (st *sourceControlState) writable() bool {
	w := st.writeState()
	// Loading is deliberately excluded: the 5s status read must not flip
	// every button's enabled state while it is in flight. A write validates
	// its workspace again on its own (scMutate re-hydrates and checks root).
	return st.Root != "" && st.IsRepo && st.Error == "" && (w == nil || (!w.Busy && !w.Unknown))
}

func (a *app) scLoadDiff() {
	st := &a.sourceControl
	f := st.selectedFile()
	if f == nil || st.Root == "" {
		st.Reader = reviewReader{Generation: st.Reader.Generation + 1}
		return
	}
	file, root, scope := *f, st.Root, st.Scope
	contextLines := max(st.Reader.Context, 3)
	// The daemon rejects a single-file diff for the committed and branch
	// ranges; those sources read a whole-source snapshot on the review
	// surface and never reach this request.
	source := "unstaged"
	switch st.Source {
	case reviewSourceStaged:
		source = "staged"
	default:
		if file.Group == "staged" {
			source = "staged"
		}
	}
	key := fmt.Sprintf("%s\x00%s\x00%s\x00%d", source, file.Group, file.Path, contextLines)
	if st.Reader.Loading && st.Reader.Key == key {
		return
	}
	st.Reader.Generation++
	gen := st.Reader.Generation
	layout := a.sourceControl.LayoutSel == 1
	if st.Reader.Key != key {
		st.Reader.Viewer = nil
		st.Reader.Note = ""
	}
	st.Reader.Key, st.Reader.Loading, st.Reader.Error = key, true, ""
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		var origin any
		if file.Origin != "" {
			origin = file.Origin
		}
		var response reviewDiffResponse
		err := scRequest(ctx, scope.Client, map[string]any{
			"type": "collectReviewFileDiff", "cwd": root, "source": source, "path": file.Path,
			"origin_path": origin, "untracked": file.Group == "untracked", "context_lines": contextLines,
		}, &response)
		var reader reviewReader
		if err == nil {
			var returnedSource string
			if json.Unmarshal(response.Data.Source, &returnedSource) != nil || returnedSource != source || response.Type != "reviewDiff" || response.Data.File == nil || response.Data.File.Path != file.Path {
				err = errors.New("Invalid file diff response")
			} else {
				reader, err = reviewBuildReader(response.Data, file.Path, contextLines, layout)
				reader.CanExpand = contextLines < 100000
			}
		}
		a.post(func() {
			st := &a.sourceControl
			if !a.scCurrent(scope) || st.Scope != scope || st.Root != root || st.Reader.Generation != gen || st.Selected != file.key() {
				return
			}
			if err != nil {
				st.Reader.Loading = false
				// Keep the diff already on screen; a transient failure must
				// not blank it every refresh. A load with nothing to show
				// reports the error instead.
				if st.Reader.Viewer == nil {
					st.Reader.Error = err.Error()
				}
				return
			}
			if st.Reader.Viewer != nil && st.Reader.Hash == reader.Hash && st.Reader.Key == key {
				st.Reader.Loading = false
				return
			}
			reader.Generation = gen
			reader.Key = key
			reader.Split = a.sourceControl.LayoutSel == 1
			if reader.Viewer != nil && reader.Split {
				reader.Viewer.Layout = editor.DiffSplit
			}
			st.Reader = reader
		})
	}()
}

func (a *app) scMutate(label string, operation map[string]any) {
	st := &a.sourceControl
	if a.review.Active || !a.scCurrent(st.Scope) || !st.writable() {
		return
	}
	scope, root, key := st.Scope, st.Root, st.writeKey()
	if st.Writes == nil {
		st.Writes = make(map[scWriteKey]*scWriteState)
	}
	write := &scWriteState{Busy: true, Label: label}
	st.Writes[key] = write
	st.Notice = ""
	operation["cwd"] = root
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		var response struct {
			Type string `json:"type"`
		}
		_, currentRoot, err := scHydrate(ctx, scope)
		if err == nil && currentRoot != root {
			err = errors.New("Workspace changed. Refresh before another write.")
		}
		unknown := false
		if err == nil {
			err = scRequest(ctx, scope.Client, operation, &response)
			unknown = scWriteUnknown(err)
		}
		if err == nil && response.Type != "ack" && response.Type != "branchChanged" {
			err = errors.New("Git result unknown. Refresh before another write.")
			unknown = true
		}
		a.post(func() {
			st := &a.sourceControl
			if st.Writes[key] != write {
				return
			}
			write.Busy, write.Unknown = false, unknown
			if !a.scCurrent(scope) || st.Scope != scope || st.Root != root {
				return
			}
			if err != nil {
				st.Notice = err.Error()
			} else {
				st.Notice = trf("%s completed", label)
				if operation["type"] == "commit" {
					st.CommitMessage = ""
				}
			}
			st.Discard = nil
			st.BranchOpen = false
			// Supersede an older in-flight status read after a write.
			st.Generation++
			st.Loading = false
			a.scRefresh(false)
		})
	}()
}

func scWriteUnknown(err error) bool {
	if err == nil {
		return false
	}
	text := strings.ToLower(err.Error())
	if strings.Contains(text, "not sent") {
		return false
	}
	if strings.Contains(text, "timed out") || strings.Contains(text, "timeout") || strings.Contains(text, "deadline") {
		return true
	}
	return !strings.Contains(text, "daemon request ") || !strings.Contains(text, " failed:")
}

func (a *app) scStage(file scFile, unstage bool) {
	st := &a.sourceControl
	current := st.selectedFile()
	if current == nil || current.key() != file.key() {
		return
	}
	paths := []string{file.Path}
	if file.Origin != "" {
		paths = append(paths, file.Origin)
	}
	for _, path := range paths {
		if !scValidPath(path) {
			st.Notice = "Unsupported path"
			return
		}
	}
	op, label := "stagePaths", "Stage"
	if unstage {
		op, label = "unstagePaths", "Unstage"
	}
	a.scMutate(label, map[string]any{"type": op, "paths": paths})
}

func (a *app) scCommit(includeUnstaged, push bool) {
	st := &a.sourceControl
	if strings.TrimSpace(st.CommitMessage) == "" {
		return
	}
	a.scMutate("Commit", map[string]any{"type": "commit", "message": st.CommitMessage, "include_unstaged": includeUnstaged, "push": push})
}

func (a *app) scConfirmDiscard() {
	st := &a.sourceControl
	if st.Discard == nil || !scValidPath(st.Discard.Path) {
		return
	}
	file := *st.Discard
	if file.Group == "staged" || file.Origin != "" {
		st.Notice = "Unstage or resolve this rename before discarding"
		st.Discard = nil
		return
	}
	tracked, untracked := []string{}, []string{}
	if file.Group == "untracked" {
		untracked = append(untracked, file.Path)
	} else {
		tracked = append(tracked, file.Path)
	}
	a.scMutate("Discard", map[string]any{"type": "discardWorktreePaths", "tracked": tracked, "untracked": untracked})
}

func (a *app) scLoadBranches() {
	st := &a.sourceControl
	if st.BranchLoading || !st.writable() {
		return
	}
	st.BranchOpen, st.BranchLoading = true, true
	st.BranchGeneration++
	scope, root, gen := st.Scope, st.Root, st.BranchGeneration
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		var response struct {
			Type     string `json:"type"`
			Snapshot *struct {
				Branches []scBranch `json:"branches"`
			} `json:"snapshot"`
		}
		err := scRequest(ctx, scope.Client, map[string]any{"type": "inspectBranchRefs", "cwd": root}, &response)
		if err == nil && (response.Type != "branches" || response.Snapshot == nil) {
			err = errors.New("Branches unavailable")
		}
		a.post(func() {
			st := &a.sourceControl
			if !a.scCurrent(scope) || st.Scope != scope || st.Root != root || st.BranchGeneration != gen {
				return
			}
			st.BranchLoading = false
			if err != nil {
				st.Notice = err.Error()
				return
			}
			st.Branches = response.Snapshot.Branches
			st.filterBranches()
		})
	}()
}

func (st *sourceControlState) filterBranches() {
	st.BranchRows = st.BranchRows[:0]
	query := strings.ToLower(st.BranchFilter)
	for _, branch := range st.Branches {
		if strings.Contains(strings.ToLower(branch.Name), query) {
			st.BranchRows = append(st.BranchRows, branch)
		}
	}
}

func projectBaseName(path string) string { return filepath.Base(filepath.Clean(path)) }

func scGroupLabel(group string) string {
	switch group {
	case "staged":
		return tr("Staged")
	case "changes":
		return tr("Unstaged")
	case "untracked":
		return tr("Untracked")
	case "ignored":
		return tr("Ignored")
	default:
		return tr("Checkpoint")
	}
}

func scFileLabel(f scFile) string {
	if f.Origin != "" {
		return fmt.Sprintf("%s → %s", f.Origin, f.Path)
	}
	return f.Path
}
