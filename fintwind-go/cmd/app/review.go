package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"

	"github.com/egoist/mygo/ui"
	"github.com/egoist/mygo/ui/editor"
	"github.com/google/uuid"
)

// reviewKind tells which whole-source snapshot the review surface shows: the
// checkpoint of one turn, or a committed / branch range of the workspace.
// Both load with one collectReviewDiff call and move between files without
// touching Git again, mirroring Rust's LastTurn review page.
type reviewKind uint8

const (
	reviewKindTurn reviewKind = iota
	reviewKindSource
)

type reviewState struct {
	Active       bool
	Kind         reviewKind
	Source       reviewSource // reviewKindSource: committed or branch
	Scope        scScope
	TurnID, Root string
	Error        string
	TurnCount    int
	Generation   uint64
	Loading      bool
	Files, Rows  []scFile
	// Chunks hold each file's patch text, sharing the snapshot's bytes;
	// hunks are parsed into Patches only when a file is shown.
	Chunks         map[string]string
	Patches        map[string]editor.FilePatch
	Complete       bool
	Truncated      bool
	Selected       string
	Filter         string
	AppliedFilter  string
	FileList       ui.ListState
	FileIndex      int
	Reader         reviewReader
	SourceMenuOpen bool
	Stats          reviewAreaStats
	StatsGen       uint64
	StatsLoading   bool
}

func (a *app) openTurnReview(turnID string) {
	scope := a.scScope()
	if scope.Client == nil || a.detail == nil || a.detail.ID != scope.SessionID {
		return
	}
	turn := a.detail.TurnByID(turnID)
	if turn == nil {
		return
	}
	// sourceControl.LayoutSel is the one layout switch for every surface,
	// like Rust's source_control_diff_split; readers seed from it so the
	// segmented control never disagrees with the rendered layout.
	split := a.sourceControl.LayoutSel == 1
	a.review = reviewState{Active: true, Kind: reviewKindTurn, Scope: scope, TurnID: turnID, TurnCount: turn.TurnCount, Generation: a.review.Generation + 1, Loading: true, Reader: reviewReader{Split: split}}
	a.mode = modeSourceControl
	a.reviewLoad()
}

// openSourceReview shows the whole-source diff of the committed or branch
// range, like Rust's set_review_diff_source for the non-live sources: one
// snapshot for every file, read-only, no checkpoint validation.
func (a *app) openSourceReview(source reviewSource) {
	scope := a.scScope()
	if scope.Client == nil || (source != reviewSourceCommitted && source != reviewSourceBranch) {
		return
	}
	split := a.sourceControl.LayoutSel == 1
	a.review = reviewState{Active: true, Kind: reviewKindSource, Source: source, Scope: scope, Generation: a.review.Generation + 1, Loading: true, Reader: reviewReader{Split: split}}
	a.mode = modeSourceControl
	a.reviewLoad()
}

func (a *app) reviewCurrent(scope scScope, turnID string, gen uint64) bool {
	return a.review.Active && a.review.Scope == scope && a.review.TurnID == turnID && a.review.Generation == gen && a.scCurrent(scope)
}

func (a *app) reviewLoad() {
	st := &a.review
	if !st.Active || !a.scCurrent(st.Scope) {
		return
	}
	st.Generation++
	scope, turnID, gen := st.Scope, st.TurnID, st.Generation
	kind, source, split := st.Kind, st.Source, a.sourceControl.LayoutSel == 1
	st.Loading, st.Error = true, ""
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		session, root, err := scHydrate(ctx, scope)
		count := 0
		var files []scFile
		if err == nil {
			switch kind {
			case reviewKindTurn:
				err = reviewTurnFiles(session, scope, turnID, &count, &files)
			case reviewKindSource:
				if root == "" {
					err = errors.New(tr("Workspace path is unavailable"))
				}
			}
		}
		var response reviewDiffResponse
		if err == nil {
			var sourceWire any
			switch kind {
			case reviewKindTurn:
				sourceWire = map[string]any{"lastTurn": map[string]any{"session_id": scope.SessionID, "turn_id": turnID, "turn_count": count}}
			case reviewKindSource:
				sourceWire = string(source)
			}
			err = scRequest(ctx, scope.Client, map[string]any{"type": "collectReviewDiff", "cwd": root, "source": sourceWire}, &response)
		}
		var chunks map[string]string
		var patches map[string]editor.FilePatch
		if err == nil {
			if err = reviewVerifySnapshot(&response, kind, source, scope.SessionID, turnID, count); err == nil {
				chunks, err = reviewSnapshotFiles(&response, kind, &files)
			}
		}
		var reader reviewReader
		if err == nil && len(files) > 0 {
			// Only the first file's hunks are parsed here; the rest wait in
			// their chunks until shown.
			if chunk, ok := chunks[files[0].Path]; ok {
				if parsed, perr := editor.ParsePatch(chunk); perr == nil && len(parsed) > 0 {
					patches = map[string]editor.FilePatch{files[0].Path: parsed[0]}
					reader = reviewReaderForPatch(parsed[0], response.Data.CompleteContext, response.Data.Truncated, 3, split)
				} else {
					reader.Error = tr("Could not parse diff")
				}
			} else {
				reader.Error = tr("This checkpoint has file metadata but no matching patch")
			}
		}
		a.post(func() {
			if !a.reviewCurrent(scope, turnID, gen) {
				return
			}
			st := &a.review
			st.Loading = false
			if err != nil {
				st.Error = err.Error()
				st.Files, st.Rows, st.Patches, st.Chunks = nil, nil, nil, nil
				st.Selected = ""
				st.Reader = reviewReader{Split: a.sourceControl.LayoutSel == 1}
				return
			}
			st.Root, st.TurnCount, st.Files = root, count, files
			st.Chunks, st.Patches = chunks, patches
			st.Complete, st.Truncated = response.Data.CompleteContext, response.Data.Truncated
			st.Selected = ""
			if len(files) > 0 {
				st.Selected = files[0].Path
			}
			st.rebuildRows()
			st.Reader = reader
			a.reviewLoadStats()
		})
	}()
}

// reviewTurnFiles validates one turn's checkpoint and lists its files, the
// load half of Rust's LastTurn review: the checkpoint must be ready and its
// ref must name this session and turn.
func reviewTurnFiles(session *scWireSession, scope scScope, turnID string, count *int, files *[]scFile) error {
	if session == nil {
		return errors.New(tr("Session checkpoint unavailable"))
	}
	found := false
	for _, turn := range session.Turns {
		if turn.ID != turnID {
			continue
		}
		found = true
		*count = turn.TurnCount
		checkpoint := turn.Checkpoint
		id, idErr := uuid.Parse(scope.SessionID)
		if idErr != nil || *count <= 0 || checkpoint == nil || checkpoint.Status != "ready" || checkpoint.TurnCount != *count || checkpoint.GitRef != fmt.Sprintf("refs/fintwind/session-%s-turn-%d", id, *count) {
			return errors.New(tr("This turn's checkpoint is unavailable or no longer valid"))
		}
		for _, f := range checkpoint.Files {
			if !scValidPath(f.Path) {
				return errors.New(tr("Snapshot contains an unsupported path"))
			}
			*files = append(*files, scFile{Path: f.Path, Group: "checkpoint", Letter: "M", Additions: f.Additions, Deletions: f.Deletions})
		}
		break
	}
	if !found {
		return errors.New(tr("This turn does not belong to the selected session"))
	}
	return nil
}

// reviewVerifySnapshot checks that the snapshot response answers this load:
// a whole-source reply echoes its source, a turn reply echoes the turn.
func reviewVerifySnapshot(response *reviewDiffResponse, kind reviewKind, source reviewSource, sessionID, turnID string, count int) error {
	if response.Type != "reviewDiff" {
		return errors.New(tr("Invalid review diff response"))
	}
	switch kind {
	case reviewKindTurn:
		var echo struct {
			LastTurn *struct {
				SessionID string `json:"session_id"`
				TurnID    string `json:"turn_id"`
				TurnCount int    `json:"turn_count"`
			} `json:"lastTurn"`
		}
		if json.Unmarshal(response.Data.Source, &echo) != nil || echo.LastTurn == nil || echo.LastTurn.SessionID != sessionID || echo.LastTurn.TurnID != turnID || echo.LastTurn.TurnCount != count {
			return errors.New(tr("Snapshot response identity does not match this turn"))
		}
	case reviewKindSource:
		var echo string
		if json.Unmarshal(response.Data.Source, &echo) != nil || echo != string(source) {
			return errors.New(tr("Snapshot response identity does not match this source"))
		}
	}
	return nil
}

// reviewSnapshotFiles splits the snapshot's patch into per-file chunks and
// lists its files. A chunk shares the patch's bytes; only its header lines
// are scanned here, and a file's hunks are parsed when it is shown. A turn
// snapshot keeps its checkpoint list, corrected by each chunk's header; a
// committed or branch snapshot is built from the chunks, with totals from
// the numstat.
func reviewSnapshotFiles(response *reviewDiffResponse, kind reviewKind, files *[]scFile) (map[string]string, error) {
	patch := response.Data.Patch
	chunks := map[string]string{}
	var stats map[string][2]uint64
	if kind == reviewKindSource {
		stats = reviewNumstat(response.Data.Numstat)
	}
	var starts []int
	if strings.HasPrefix(patch, "diff --git ") {
		starts = append(starts, 0)
	}
	for from := 0; ; {
		i := strings.Index(patch[from:], "\ndiff --git ")
		if i < 0 {
			break
		}
		starts = append(starts, from+i+1)
		from += i + 1
	}
	for i, start := range starts {
		end := len(patch)
		if i+1 < len(starts) {
			end = starts[i+1]
		}
		chunk := patch[start:end]
		f, ok := reviewFileFromChunk(chunk)
		if !ok {
			continue
		}
		if !scValidPath(f.Path) {
			return nil, errors.New(tr("Snapshot diff contains an unsupported path"))
		}
		f.Group = "checkpoint"
		if kind == reviewKindSource {
			f.Additions, f.Deletions = stats[f.Path][0], stats[f.Path][1]
			if f.Origin != "" && f.Origin != f.Path {
				f.Additions += stats[f.Origin][0]
				f.Deletions += stats[f.Origin][1]
			}
			*files = append(*files, f)
		} else {
			for j := range *files {
				if (*files)[j].Path == f.Path {
					(*files)[j].Letter, (*files)[j].Origin, (*files)[j].Binary = f.Letter, f.Origin, f.Binary
					break
				}
			}
		}
		chunks[f.Path] = chunk
	}
	return chunks, nil
}

// reviewFileFromChunk reads one file's identity from the header lines of its
// chunk, before the first hunk: path, rename origin, status letter, binary.
func reviewFileFromChunk(chunk string) (f scFile, ok bool) {
	var oldName, newName string
	rest := chunk
	for rest != "" {
		line := ""
		if i := strings.IndexByte(rest, '\n'); i >= 0 {
			line, rest = rest[:i], rest[i+1:]
		} else {
			line, rest = rest, ""
		}
		line = strings.TrimSuffix(line, "\r")
		if strings.HasPrefix(line, "@@ ") {
			break
		}
		switch {
		case strings.HasPrefix(line, "new file mode "):
			f.Letter = "A"
		case strings.HasPrefix(line, "deleted file mode "):
			f.Letter = "D"
		case strings.HasPrefix(line, "rename from "):
			f.Origin = unquoteGitPath(line[len("rename from "):])
		case strings.HasPrefix(line, "rename to "):
			f.Path = unquoteGitPath(line[len("rename to "):])
		case strings.HasPrefix(line, "copy from "):
			f.Origin = unquoteGitPath(line[len("copy from "):])
		case strings.HasPrefix(line, "copy to "):
			f.Path = unquoteGitPath(line[len("copy to "):])
		case strings.HasPrefix(line, "Binary files ") || line == "GIT binary patch":
			f.Binary = true
		case strings.HasPrefix(line, "--- "):
			oldName = reviewHeaderPath(line[4:], "a/")
		case strings.HasPrefix(line, "+++ "):
			newName = reviewHeaderPath(line[4:], "b/")
		}
	}
	if f.Path == "" {
		f.Path = newName
	}
	if f.Origin == "" {
		f.Origin = oldName
	}
	switch {
	case f.Letter != "":
	case oldName == "" && newName != "":
		f.Letter = "A"
	case newName == "" && oldName != "":
		f.Letter = "D"
	case oldName != newName && oldName != "" && newName != "":
		f.Letter = "R"
	default:
		f.Letter = "M"
	}
	return f, f.Path != ""
}

// reviewHeaderPath reads a "--- a/path" or "+++ b/path" header name:
// /dev/null means the side has no file, and git's quoted paths unescape.
func reviewHeaderPath(rest, prefix string) string {
	if rest == "/dev/null" {
		return ""
	}
	if strings.HasPrefix(rest, prefix) {
		rest = rest[len(prefix):]
	}
	return unquoteGitPath(rest)
}

// unquoteGitPath unwraps git's quoted path form and its backslash escapes.
func unquoteGitPath(path string) string {
	if len(path) < 2 || path[0] != '"' || path[len(path)-1] != '"' {
		return path
	}
	path = path[1 : len(path)-1]
	var b strings.Builder
	for i := 0; i < len(path); i++ {
		if path[i] == '\\' && i+1 < len(path) {
			i++
			switch path[i] {
			case 't':
				b.WriteByte('\t')
			case 'n':
				b.WriteByte('\n')
			case 'r':
				b.WriteByte('\r')
			case '\\':
				b.WriteByte('\\')
			case '"':
				b.WriteByte('"')
			default:
				b.WriteByte(path[i])
			}
			continue
		}
		b.WriteByte(path[i])
	}
	return b.String()
}

// reviewNumstat reads a git diff --numstat body into per-path totals, for
// the "+" and "−" columns of a whole-source file list.
func reviewNumstat(numstat string) map[string][2]uint64 {
	stats := map[string][2]uint64{}
	for _, line := range strings.Split(numstat, "\n") {
		columns := strings.SplitN(line, "\t", 3)
		if len(columns) < 3 {
			continue
		}
		add, _ := parseCount(columns[0])
		del, _ := parseCount(columns[1])
		stats[strings.TrimSuffix(columns[2], "\r")] = [2]uint64{add, del}
	}
	return stats
}

func parseCount(text string) (uint64, error) {
	return strconv.ParseUint(strings.TrimSpace(text), 10, 64)
}

func (st *reviewState) rebuildRows() {
	st.AppliedFilter = st.Filter
	st.Rows = st.Rows[:0]
	for _, f := range st.Files {
		if strings.Contains(strings.ToLower(f.Path), strings.ToLower(st.Filter)) {
			st.Rows = append(st.Rows, f)
		}
	}
	st.FileIndex = -1
	for i, row := range st.Rows {
		if row.Path == st.Selected {
			st.FileIndex = i
			break
		}
	}
}

// reviewSelect shows one snapshot file. The patch is already in memory, so
// this never asks Git: a complete snapshot rebuilds whole texts and expands
// context in place, a truncated one shows what the daemon kept. A file not
// shown before has its chunk parsed here, then stays cached.
func (a *app) reviewSelect(path string, contextLines int) {
	st := &a.review
	if !st.Active || !a.scCurrent(st.Scope) {
		return
	}
	patch, havePatch := st.Patches[path]
	chunk := ""
	if !havePatch {
		var haveChunk bool
		chunk, haveChunk = st.Chunks[path]
		if !haveChunk {
			st.Selected = path
			st.Reader = reviewReader{Generation: st.Reader.Generation + 1, Split: a.sourceControl.LayoutSel == 1, Error: tr("No patch is available for this checkpoint file")}
			return
		}
	}
	st.Selected = path
	st.Reader.Generation++
	scope, turnID, gen, readerGen := st.Scope, st.TurnID, st.Generation, st.Reader.Generation
	split, complete, truncated := a.sourceControl.LayoutSel == 1, st.Complete, st.Truncated
	st.Reader.Loading, st.Reader.Viewer, st.Reader.Error = true, nil, ""
	go func() {
		if !havePatch {
			parsed, err := editor.ParsePatch(chunk)
			if err != nil || len(parsed) == 0 {
				a.post(func() {
					if !a.reviewCurrent(scope, turnID, gen) || a.review.Selected != path || a.review.Reader.Generation != readerGen {
						return
					}
					a.review.Reader.Loading = false
					a.review.Reader.Error = tr("Could not parse diff")
				})
				return
			}
			patch = parsed[0]
		}
		reader := reviewReaderForPatch(patch, complete, truncated, contextLines, split)
		a.post(func() {
			if !a.reviewCurrent(scope, turnID, gen) || a.review.Selected != path || a.review.Reader.Generation != readerGen {
				return
			}
			if !havePatch {
				if a.review.Patches == nil {
					a.review.Patches = map[string]editor.FilePatch{}
				}
				a.review.Patches[path] = patch
			}
			reader.Generation = readerGen
			a.review.Reader = reader
		})
	}()
}

func (a *app) reviewBackToLive() {
	a.review.Generation++
	a.review.Active = false
	a.review.SourceMenuOpen = false
	a.sourceControl.LastRefresh = a.sourceControl.LastRefresh.Add(-scRefreshInterval)
	a.scEnsureFresh()
}

func (a *app) openSourceControl() {
	a.mode = modeSourceControl
	a.reviewBackToLive()
}

func (a *app) reviewBackToSession() {
	a.review.Generation++
	a.review.Active = false
	a.review.SourceMenuOpen = false
	a.mode = modeSessions
}

func (a *app) reviewHighlighted(relative string) bool {
	if !a.review.Active || !a.scCurrent(a.review.Scope) {
		return false
	}
	for _, file := range a.review.Files {
		if file.Path == relative {
			return true
		}
	}
	return false
}

// reviewLoadStats fetches the toolbar totals of the snapshot's source, the
// review-side twin of scLoadStats. It only runs from a snapshot load, never
// from the render path.
func (a *app) reviewLoadStats() {
	st := &a.review
	if !st.Active || !a.scCurrent(st.Scope) || st.Root == "" {
		return
	}
	scope, root, turnID := st.Scope, st.Root, st.TurnID
	kind, source, count := st.Kind, st.Source, st.TurnCount
	stateGen, statsGen := st.Generation, st.StatsGen+1
	st.StatsGen = statsGen
	st.StatsLoading = true
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), scGitTimeout)
		defer cancel()
		var wire any
		switch kind {
		case reviewKindTurn:
			wire = map[string]any{"lastTurn": map[string]any{"session_id": scope.SessionID, "turn_id": turnID, "turn_count": count}}
		case reviewKindSource:
			wire = string(source)
		}
		var response struct {
			Type      string `json:"type"`
			Additions uint64 `json:"additions"`
			Deletions uint64 `json:"deletions"`
			Files     uint64 `json:"files"`
			Truncated bool   `json:"truncated"`
		}
		err := scRequest(ctx, scope.Client, map[string]any{"type": "collectReviewStats", "cwd": root, "source": wire}, &response)
		a.post(func() {
			st := &a.review
			if !a.reviewCurrent(scope, turnID, stateGen) || st.StatsGen != statsGen {
				return
			}
			st.StatsLoading = false
			if err != nil || response.Type != "reviewStats" {
				st.Stats = reviewAreaStats{}
				return
			}
			st.Stats = reviewAreaStats{Additions: response.Additions, Deletions: response.Deletions, Files: response.Files, Truncated: response.Truncated, Valid: response.Additions > 0 || response.Deletions > 0}
		})
	}()
}

// reviewStep moves the snapshot reader to the previous/next file, mirroring
// step_source_control_file for history snapshots.
func (a *app) reviewStep(delta int) {
	st := &a.review
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
	a.reviewSelect(st.Rows[next].Path, 3)
}
