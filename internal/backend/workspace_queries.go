package backend

import (
	"encoding/json"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/backendworkspace"
)

// Only read operations with explicit cancellation identities leave the ordered
// request loop. Mutations and runtime commands retain their existing ordering.
func cancellableWorkspaceQuery(command backendstore.Object) (backendstore.Object, bool, error) {
	if command.String("type") != "workspace" {
		return nil, false, nil
	}
	raw, err := backendworkspace.NormalizeOperation(command["operation"])
	if err != nil {
		return nil, false, err
	}
	op, err := backendstore.Decode(json.RawMessage(raw))
	if err != nil {
		return nil, false, err
	}
	if op.String("type") == "cancelGitQuery" {
		return op, true, nil
	}
	if op.String("queryId") == "" {
		return nil, false, nil
	}
	switch op.String("type") {
	case "inspectStatus", "inspectTrackedStatus", "listUntracked", "listIgnored", "discoverNestedRepositories", "collectReviewDiff", "collectReviewFileDiff", "collectReviewStats", "inspectBranches", "inspectCommit":
		return op, true, nil
	}
	return nil, false, nil
}

func sendQueryResponse(p *peer, requestID string, result backendstore.Object, err error) bool {
	outcome := map[string]any{"status": "ok", "payload": result}
	if err != nil {
		outcome = map[string]any{"status": "error", "error": map[string]string{"message": err.Error()}}
	}
	return p.send(map[string]any{"type": "response", "requestId": requestID, "outcome": outcome})
}

// Serialize admission with Close so no WaitGroup.Add can race shutdown's Wait.
// The slot is shared with every ordinary request, not multiplied per connection.
func (s *Server) admitWorkspaceQuery() bool {
	s.queryAdmissionMu.Lock()
	defer s.queryAdmissionMu.Unlock()
	if s.queryClosed {
		return false
	}
	select {
	case s.workers <- struct{}{}:
		s.queryWork.Add(1)
		return true
	default:
		return false
	}
}
