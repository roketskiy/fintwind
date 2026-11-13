package model

import (
	"fmt"
	"strings"
	"time"

	"github.com/google/uuid"
)

// Submission states, mirroring submission.rs SubmissionState camelCase.
const (
	SubmissionPreparing   = "preparing"
	SubmissionDispatching = "dispatching"
	SubmissionAccepted    = "accepted"
	SubmissionUnknown     = "unknown"
	SubmissionNotSent     = "notSent"
	SubmissionRejected    = "rejected"
)

// SubmissionReceipt is the daemon-owned admission fact for one turn. The id
// is the local turn id and is never regenerated for an RPC retry.
type SubmissionReceipt struct {
	ID              string  `json:"id"`
	InputID         string  `json:"input_id"`
	State           string  `json:"state"`
	CreatedAt       uint64  `json:"created_at"`
	UpdatedAt       uint64  `json:"updated_at"`
	NativeSessionID *string `json:"native_session_id"`
}

// Unconfirmed reports a claim without proof of execution. Unknown stays
// blocking until reconciled; it is never proof of execution.
func (r SubmissionReceipt) Unconfirmed() bool {
	return r.State == SubmissionDispatching || r.State == SubmissionUnknown
}

// Terminal reports a receipt the daemon proved will never execute.
func (r SubmissionReceipt) Terminal() bool {
	return r.State == SubmissionNotSent || r.State == SubmissionRejected
}

// SubmissionFile is one daemon-side prompt file: path, display name and
// the durable blob reference the daemon attributed at accept time.
type SubmissionFile struct {
	Path          string  `json:"path"`
	Name          string  `json:"name"`
	BlobReference *string `json:"blob_reference,omitempty"`
}

// SubmissionRecord freezes one send: the session, the admission receipt,
// the provider-facing text and the files. The daemon refuses a second
// accept whose identity matches but whose prompt or files differ.
type SubmissionRecord struct {
	SessionID string            `json:"session_id"`
	Receipt   SubmissionReceipt `json:"receipt"`
	Prompt    string            `json:"prompt"`
	Files     []SubmissionFile  `json:"files"`
}

// NewSubmissionRecord builds a Preparing record with the daemon's sortable
// input id. Files stays an array on the wire: the daemon compares the saved
// record by value and null never equals [].
func NewSubmissionRecord(sessionID, turnID, prompt string, files []SubmissionFile) SubmissionRecord {
	now := uint64(time.Now().UnixMilli())
	if files == nil {
		files = []SubmissionFile{}
	}
	return SubmissionRecord{
		SessionID: sessionID,
		Receipt: SubmissionReceipt{
			ID:        turnID,
			InputID:   NewSubmissionInputID(now),
			State:     SubmissionPreparing,
			CreatedAt: now,
			UpdatedAt: now,
		},
		Prompt: prompt,
		Files:  files,
	}
}

// NewSubmissionInputID ports submission.rs new_input_id: the millisecond
// timestamp scaled into the sortable prefix plus the first 14 hex digits
// of a v4 UUID. The id is persisted before any POST.
func NewSubmissionInputID(timestampMillis uint64) string {
	prefix := timestampMillis * 0x1000 & 0xffffffffffff
	suffix := strings.ReplaceAll(uuid.NewString(), "-", "")
	return fmt.Sprintf("msg_%012x%s", prefix, suffix[:14])
}
