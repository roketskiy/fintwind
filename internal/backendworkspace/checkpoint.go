package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/backendstore"
	"github.com/google/uuid"
)

const emptyGitTree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
const turnMetadataPrefix = "Fintwind-Turn-Start: "

type turnMetadata struct {
	Head   *string           `json:"head"`
	Branch *string           `json:"branch"`
	Refs   map[string]string `json:"refs"`
}

func checkpointRef(session, kind string, turn int64) string {
	return fmt.Sprintf("refs/fintwind/session-%s-turn-%s%d", session, kind, turn)
}
func gitText(ctx context.Context, cwd string, args ...string) (string, error) {
	out, err := runGit(ctx, cwd, args)
	return strings.TrimSpace(string(out.stdout)), err
}
func resolveCommit(ctx context.Context, cwd, ref string) *string {
	value, err := gitText(ctx, cwd, "rev-parse", "--verify", ref+"^{commit}")
	if err != nil || value == "" {
		return nil
	}
	return &value
}
func gitBranch(ctx context.Context, cwd string) *string {
	value, err := gitText(ctx, cwd, "symbolic-ref", "--quiet", "HEAD")
	if err != nil || value == "" {
		return nil
	}
	return &value
}
func sameString(a, b *string) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

func snapshotCommit(ctx context.Context, root *confinedRoot, cwd string, head *string, message string, parents []string) (string, error) {
	common, err := gitText(ctx, cwd, "rev-parse", "--git-common-dir")
	if err != nil {
		return "", err
	}
	if common == "" {
		return "", errors.New("git common directory missing")
	}
	if !filepath.IsAbs(common) {
		common = filepath.Join(cwd, common)
	}
	if _, _, err := root.resolveDir(common); err != nil {
		return "", errors.New("git common directory outside isolated workspace")
	}
	index, err := os.CreateTemp(common, "fintwind-checkpoint-index-*")
	if err != nil {
		return "", err
	}
	path := index.Name()
	if err := index.Close(); err != nil {
		return "", err
	}
	if err := os.Remove(path); err != nil {
		return "", err
	}
	defer os.Remove(path)
	defer os.Remove(path + ".lock")
	env := map[string]string{"GIT_INDEX_FILE": path}
	if head != nil {
		if _, err := runGitEnvironment(ctx, cwd, []string{"read-tree", *head}, env); err != nil {
			return "", err
		}
	}
	if _, err := runGitEnvironment(ctx, cwd, []string{"add", "-A", "--", "."}, env); err != nil {
		return "", err
	}
	tree, err := runGitEnvironment(ctx, cwd, []string{"write-tree"}, env)
	if err != nil {
		return "", err
	}
	return makeCheckpointCommit(ctx, cwd, strings.TrimSpace(string(tree.stdout)), message, parents)
}

func makeCheckpointCommit(ctx context.Context, cwd, tree, message string, parents []string) (string, error) {
	args := []string{"commit-tree", tree, "-m", message}
	for _, parent := range parents {
		args = append(args, "-p", parent)
	}
	value, err := gitText(ctx, cwd, args...)
	if err != nil {
		return "", err
	}
	if value == "" {
		return "", errors.New("git commit-tree returned no object")
	}
	return value, nil
}

func turnDiffBase(ctx context.Context, cwd, session string, turn int64, start string, endHead, endBranch *string) (string, error) {
	message, err := gitText(ctx, cwd, "show", "-s", "--format=%B", start)
	if err != nil {
		return "", err
	}
	var metadata turnMetadata
	found := false
	for _, line := range strings.Split(message, "\n") {
		if strings.HasPrefix(line, turnMetadataPrefix) {
			if err := json.Unmarshal([]byte(strings.TrimPrefix(line, turnMetadataPrefix)), &metadata); err != nil {
				return "", err
			}
			found = true
			break
		}
	}
	if !found {
		return "", errors.New("turn starting checkpoint metadata unavailable")
	}
	var commit string
	sameLine := metadata.Branch != nil && endBranch != nil && *metadata.Branch == *endBranch || metadata.Branch == nil && endBranch == nil && sameString(metadata.Head, endHead)
	if sameLine {
		resolved := resolveCommit(ctx, cwd, start)
		if resolved == nil {
			return "", errors.New("starting checkpoint disappeared")
		}
		commit = *resolved
	} else {
		target := ""
		if endBranch != nil {
			target = metadata.Refs[*endBranch]
		}
		if target == "" {
			if endHead != nil {
				newCommits, err := gitText(ctx, cwd, "rev-list", "--first-parent", "--reverse", *endHead, "--not", start)
				if err != nil {
					return "", err
				}
				first, _, _ := strings.Cut(newCommits, "\n")
				if first == "" {
					target = *endHead
				} else if parent := resolveCommit(ctx, cwd, first+"^1"); parent != nil {
					target = *parent
				}
			}
			if target == "" {
				var err error
				target, err = makeCheckpointCommit(ctx, cwd, emptyGitTree, "fintwind empty turn diff base", nil)
				if err != nil {
					return "", err
				}
			}
		}
		commit = target
		if metadata.Head != nil {
			if *metadata.Head == target {
				resolved := resolveCommit(ctx, cwd, start)
				if resolved == nil {
					return "", errors.New("starting checkpoint disappeared")
				}
				commit = *resolved
			} else {
				dirty, err := gitText(ctx, cwd, "diff", "--name-only", *metadata.Head, start)
				if err != nil {
					return "", err
				}
				if dirty != "" {
					merged, err := gitText(ctx, cwd, "merge-tree", "--write-tree", "--merge-base", *metadata.Head, target, start)
					if err != nil {
						return "", err
					}
					tree, _, _ := strings.Cut(merged, "\n")
					commit, err = makeCheckpointCommit(ctx, cwd, tree, "fintwind turn diff base", nil)
					if err != nil {
						return "", err
					}
				}
			}
		}
	}
	ref := checkpointRef(session, "diff-", turn)
	if _, err := runGit(ctx, cwd, []string{"update-ref", ref, commit}); err != nil {
		return "", err
	}
	return ref, nil
}

func restoreCheckpoint(ctx context.Context, cwd, ref string) error {
	commit := resolveCommit(ctx, cwd, ref)
	if commit == nil {
		return errors.New("checkpoint unavailable")
	}
	for _, args := range [][]string{{"restore", "--source", *commit, "--worktree", "--staged", "--", "."}, {"clean", "-fd", "--", "."}} {
		if _, err := runGit(ctx, cwd, args); err != nil {
			return err
		}
	}
	if resolveCommit(ctx, cwd, "HEAD") != nil {
		_, err := runGit(ctx, cwd, []string{"reset", "--quiet", "--", "."})
		return err
	}
	return nil
}

func sessionCheckpointRefs(ctx context.Context, cwd, session string) (map[string]string, error) {
	prefix := "refs/fintwind/session-" + session + "-turn-"
	text, err := gitText(ctx, cwd, "for-each-ref", "--format=%(refname) %(objectname)", prefix+"*")
	if err != nil {
		return nil, err
	}
	result := map[string]string{}
	for _, line := range strings.Split(text, "\n") {
		ref, commit, ok := strings.Cut(line, " ")
		if !ok || !strings.HasPrefix(ref, prefix) {
			continue
		}
		suffix := strings.TrimPrefix(ref, prefix)
		suffix = strings.TrimPrefix(strings.TrimPrefix(suffix, "start-"), "diff-")
		if _, err := strconv.ParseUint(suffix, 10, 64); err == nil {
			result[ref] = commit
		}
	}
	return result, nil
}

func checkpointNumstat(ctx context.Context, cwd, from, to string) ([]map[string]any, uint64, uint64, error) {
	out, err := runGit(ctx, cwd, []string{"diff", "--numstat", "-z", "--no-renames", from, to, "--", "."})
	if err != nil {
		return nil, 0, 0, err
	}
	files := []map[string]any{}
	var additions, deletions uint64
	for _, line := range strings.Split(string(out.stdout), "\x00") {
		parts := strings.SplitN(line, "\t", 3)
		if len(parts) != 3 {
			continue
		}
		a, _ := strconv.ParseUint(parts[0], 10, 64)
		d, _ := strconv.ParseUint(parts[1], 10, 64)
		additions += a
		deletions += d
		files = append(files, map[string]any{"path": parts[2], "additions": a, "deletions": d})
	}
	return files, additions, deletions, nil
}

func dispatchCheckpoint(ctx context.Context, root *confinedRoot, raw json.RawMessage) (json.RawMessage, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	kind := c.String("type")
	switch kind {
	case "captureTurn", "captureTurnStart", "sessionTurnRefs", "deleteSessionRefs", "deleteAllSessionRefs", "deleteTurnRefsAfter", "copySessionRefs":
	default:
		return nil, false, nil
	}
	defer root.close()
	cwd, _, err := root.resolveDir(c.String("cwd"))
	if err != nil {
		return nil, true, err
	}
	session := c.String("sessionId")
	if kind == "copySessionRefs" {
		session = c.String("sourceSessionId")
	}
	if uuid.Validate(session) != nil {
		return nil, true, errors.New("valid checkpoint session required")
	}
	for _, key := range []string{"turnCount", "retainedTurnCount", "previousTurnCount", "lastTurnCount", "throughTurnCount"} {
		if c[key] != nil {
			n, err := c.Int(key)
			if err != nil || n < 0 || n > 1000000 {
				return nil, true, errors.New("invalid checkpoint turn count")
			}
		}
	}
	if kind == "copySessionRefs" && uuid.Validate(c.String("targetSessionId")) != nil {
		return nil, true, errors.New("valid target session required")
	}
	mutationGate.Lock()
	defer mutationGate.Unlock()
	top, repo, err := gitToplevel(ctx, cwd)
	if err != nil {
		return nil, true, err
	}
	if repo {
		if _, _, err := root.resolveDir(top); err != nil {
			return nil, true, errors.New("repository root outside isolated workspace")
		}
	}
	ack := func() (json.RawMessage, bool, error) {
		raw, err := encodeResult(map[string]string{"type": "ack"})
		return raw, true, err
	}
	if kind == "captureTurnStart" {
		if !repo {
			return ack()
		}
		turn, err := c.Int("turnCount")
		if err != nil {
			return nil, true, err
		}
		ref := checkpointRef(session, "start-", turn)
		metadata := turnMetadata{Head: resolveCommit(ctx, cwd, "HEAD"), Branch: gitBranch(ctx, cwd), Refs: map[string]string{}}
		refs, err := gitText(ctx, cwd, "for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/remotes")
		if err != nil {
			return nil, true, err
		}
		parents := []string{}
		seen := map[string]bool{}
		if metadata.Head != nil {
			parents = append(parents, *metadata.Head)
			seen[*metadata.Head] = true
		}
		for _, line := range strings.Split(refs, "\n") {
			name, commit, ok := strings.Cut(line, " ")
			if ok {
				metadata.Refs[name] = commit
				if !seen[commit] {
					parents = append(parents, commit)
					seen[commit] = true
				}
			}
		}
		encoded, err := json.Marshal(metadata)
		if err != nil {
			return nil, true, err
		}
		commit, err := snapshotCommit(ctx, root, cwd, metadata.Head, turnMetadataPrefix+string(encoded), parents)
		if err != nil {
			return nil, true, err
		}
		commands := fmt.Sprintf("update %s %s\n", ref, commit)
		baseline := checkpointRef(session, "", max(0, turn-1))
		if resolveCommit(ctx, cwd, baseline) == nil {
			commands += fmt.Sprintf("update %s %s\n", baseline, commit)
		}
		if _, err := runGitInput(ctx, cwd, []string{"update-ref", "--stdin"}, nil, []byte(commands)); err != nil {
			return nil, true, err
		}
		return ack()
	}
	if kind == "captureTurn" {
		turn, err := c.Int("turnCount")
		if err != nil {
			return nil, true, err
		}
		ref := checkpointRef(session, "", turn)
		checkpoint := map[string]any{"turn_count": turn, "git_ref": ref, "status": "unavailable", "files": []any{}, "additions": 0, "deletions": 0, "created_at": time.Now().Unix()}
		if repo {
			head, branch := resolveCommit(ctx, cwd, "HEAD"), gitBranch(ctx, cwd)
			commit, err := snapshotCommit(ctx, root, cwd, head, "fintwind worktree snapshot", nil)
			if err == nil {
				_, err = runGit(ctx, cwd, []string{"update-ref", ref, commit})
			}
			if err == nil {
				checkpoint["status"] = "ready"
				base := ""
				if turn > 0 {
					start := checkpointRef(session, "start-", turn)
					if resolveCommit(ctx, cwd, start) != nil {
						base, err = turnDiffBase(ctx, cwd, session, turn, start, head, branch)
					} else if previous := checkpointRef(session, "", turn-1); resolveCommit(ctx, cwd, previous) != nil {
						base = previous
					}
				}
				if err == nil && base != "" {
					var files []map[string]any
					var a, d uint64
					files, a, d, err = checkpointNumstat(ctx, cwd, base, ref)
					checkpoint["files"] = files
					checkpoint["additions"] = a
					checkpoint["deletions"] = d
				}
			}
			if err != nil {
				checkpoint["status"] = "error"
			}
		}
		response, err := encodeResult(map[string]any{"type": "checkpoint", "checkpoint": checkpoint})
		return response, true, err
	}
	refs := map[string]string{}
	if repo {
		refs, err = sessionCheckpointRefs(ctx, cwd, session)
		if err != nil {
			return nil, true, err
		}
	}
	if kind == "sessionTurnRefs" {
		turns := []uint64{}
		for ref := range refs {
			suffix := strings.TrimPrefix(ref, "refs/fintwind/session-"+session+"-turn-")
			if n, err := strconv.ParseUint(suffix, 10, 64); err == nil {
				turns = append(turns, n)
			}
		}
		response, err := encodeResult(map[string]any{"type": "turnRefs", "turn_counts": turns})
		return response, true, err
	}
	commands := strings.Builder{}
	for ref, commit := range refs {
		suffix := strings.TrimPrefix(ref, "refs/fintwind/session-"+session+"-turn-")
		number := strings.TrimPrefix(strings.TrimPrefix(suffix, "start-"), "diff-")
		turn, _ := strconv.ParseInt(number, 10, 64)
		apply := false
		switch kind {
		case "deleteAllSessionRefs", "deleteSessionRefs":
			apply = true
		case "deleteTurnRefsAfter":
			retained, _ := c.Int("retainedTurnCount")
			previous, _ := c.Int("previousTurnCount")
			apply = turn > retained && turn <= previous
		case "copySessionRefs":
			limit, _ := c.Int("throughTurnCount")
			if turn <= limit {
				target := "refs/fintwind/session-" + c.String("targetSessionId") + "-turn-" + suffix
				fmt.Fprintf(&commands, "update %s %s\n", target, commit)
			}
		}
		if apply {
			fmt.Fprintf(&commands, "delete %s\n", ref)
		}
	}
	if commands.Len() > 0 {
		if _, err := runGitInput(ctx, cwd, []string{"update-ref", "--stdin"}, nil, []byte(commands.String())); err != nil {
			return nil, true, err
		}
	}
	return ack()
}
