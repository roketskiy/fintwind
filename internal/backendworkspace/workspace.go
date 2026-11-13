// Package backendworkspace is a local-only workspace capability module.
//
// It speaks the daemon wire shapes from
// legacy/rust/crates/fintwind-protocol/src/workspace.rs (WorkspaceOperation and
// WorkspaceResult JSON with a "type" tag) but runs against one isolated
// directory tree only. The caller passes that tree as root; every local input
// path must resolve inside root. Explicit Git remote operations use a separate
// sanitized Git directory and validated targets, not repository transport
// helpers or inherited credentials. No shell interprets caller input.
//
// Files, directory navigation, repository discovery, Git reads and mutations,
// worktrees and checkpoint refs are handled here. Live command/skill registries,
// PR inspection, commit-message generation and project journals are coordinated
// by the daemon before dispatch. Unknown operations fail explicitly.
//
// Isolation and pressure rules:
//
//   - root must be an absolute local path: no UNC, no missing directory.
//   - Relative inputs accept only normal components (no ".", "..",
//     no absolute paths, no NUL), mirroring the daemon resolver, and
//     all opens go through os.Root so a symlink swap cannot escape root.
//   - File content and directory listings carry byte and entry caps;
//     over-limit reads fail instead of loading the whole file.
//   - Long walks and every Git child honor ctx cancellation.
//   - Git children run only through ownedprocess (kill-on-close Job,
//     explicit allowlisted environment, per-call timeout) with an
//     allowlisted verb set. At most four run at once.
package backendworkspace

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
)

// Operation type names this module implements. Anything else is refused.
const (
	opBrowseDirectory       = "browseDirectory"
	opReadTextFile          = "readTextFile"
	opWriteTextFile         = "writeTextFile"
	opListProjectFiles      = "listProjectFiles"
	opDiscoverSlashCommands = "discoverSlashCommands"
	opCurrentBranch         = "currentBranch"
	opInspectBranchRefs     = "inspectBranchRefs"
	opListCommits           = "listCommits"
	opReadCommitStats       = "readCommitStats"
)

// unimplementedReason names why a real protocol operation is refused here.
// New entries must use exact protocol type names; nothing is invented.
func unimplementedReason(op string) string {
	switch op {
	case "inspectPullRequests", "generateCommitMessage":
		return "requires the daemon-owned workspace service"
	case "createEmptyProject", "getProjectOperation", "createProjectlessWorkspace", "migrateProjectlessWorkspace":
		return "requires the daemon-owned project service"
	default:
		return ""
	}
}

type envelope struct {
	Type string `json:"type"`
}

// Dispatch runs one WorkspaceOperation against root and returns the
// WorkspaceResult JSON. root is the caller-owned isolation directory;
// operation carries the protocol JSON including its "type" tag.
func Dispatch(ctx context.Context, root string, operation json.RawMessage) (json.RawMessage, error) {
	return DispatchWithQueries(ctx, root, operation, nil)
}

func DispatchWithQueries(ctx context.Context, root string, operation json.RawMessage, queries *Queries) (json.RawMessage, error) {
	var normalizeErr error
	operation, normalizeErr = normalizeOperation(operation)
	if normalizeErr != nil {
		return nil, normalizeErr
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	var env envelope
	if err := json.Unmarshal(operation, &env); err != nil {
		return nil, fmt.Errorf("decode operation type: %w", err)
	}
	if strings.TrimSpace(env.Type) == "" {
		return nil, fmt.Errorf("operation is missing its type tag")
	}
	confined, err := confineRoot(root)
	if err != nil {
		return nil, err
	}
	defer confined.close()
	switch env.Type {
	case "browseDirectories":
		return dispatchBrowseDirectories(ctx, confined, operation)
	case opBrowseDirectory:
		return dispatchBrowseDirectory(ctx, confined, operation)
	case opReadTextFile:
		return dispatchReadTextFile(ctx, confined, operation)
	case opWriteTextFile:
		return dispatchWriteTextFile(ctx, confined, operation)
	case opListProjectFiles:
		return dispatchListProjectFiles(ctx, confined, operation)
	case opDiscoverSlashCommands:
		return dispatchDiscoverSlashCommands(ctx, confined, operation)
	case opCurrentBranch:
		return dispatchCurrentBranch(ctx, confined, operation)
	case opInspectBranchRefs:
		return dispatchInspectBranchRefs(ctx, confined, operation)
	case opListCommits:
		return dispatchListCommits(ctx, confined, operation)
	case opReadCommitStats:
		return dispatchReadCommitStats(ctx, confined, operation)
	default:
		if result, handled, err := dispatchStatus(ctx, confined, operation, queries); handled {
			return result, err
		}
		if env.Type == "discoverNestedRepositories" {
			return dispatchNestedRepositories(ctx, confined, operation, queries)
		}
		if result, handled, err := dispatchWorktree(ctx, confined, operation); handled {
			return result, err
		}
		if result, handled, err := dispatchListings(ctx, confined, operation, queries); handled {
			return result, err
		}
		if result, handled, err := dispatchReview(ctx, confined, operation, queries); handled {
			return result, err
		}
		if result, handled, err := dispatchCheckpoint(ctx, confined, operation); handled {
			return result, err
		}
		if result, handled, err := dispatchGitMutation(ctx, confined, operation); handled {
			return result, err
		}
		if reason := unimplementedReason(env.Type); reason != "" {
			return nil, fmt.Errorf("unsupported operation %q: %s", env.Type, reason)
		}
		return nil, fmt.Errorf("unknown operation type %q", env.Type)
	}
}

func encodeResult(value any) (json.RawMessage, error) {
	raw, err := json.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("encode result: %w", err)
	}
	return raw, nil
}

func decodeOperation(operation json.RawMessage, value any, op string) error {
	dec := json.NewDecoder(strings.NewReader(string(operation)))
	dec.DisallowUnknownFields()
	if err := dec.Decode(value); err != nil {
		return fmt.Errorf("decode %s: %w", op, err)
	}
	return nil
}
