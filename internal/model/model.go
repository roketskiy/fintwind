// Package model holds the daemon wire types this client reads.
package model

import (
	"encoding/json"
	"time"
)

// TaskState is the daemon's catalog of projects and sessions.
type TaskState struct {
	Projects              []Project `json:"projects"`
	Sessions              []Session `json:"sessions"`
	DefaultCwd            string    `json:"defaultCwd"`
	ProjectlessRoot       string    `json:"projectlessRoot"`
	ProjectlessLegacyRoot string    `json:"projectlessLegacyRoot"`
}

// Project is a workspace the user added. Session fields are snake_case on the
// wire because AgentSession and Project carry no rename attribute.
type Project struct {
	ID            string `json:"id"`
	Name          string `json:"name"`
	Path          string `json:"path"`
	EnvironmentID string `json:"environment_id"`
	CreatedAt     uint64 `json:"created_at"`
}

// Session is one task. A list projection has no messages; call HydrateSession
// for the transcript.
type Session struct {
	ID              string           `json:"id"`
	Title           string           `json:"title"`
	AutoTitle       *string          `json:"auto_title"`
	ProjectID       string           `json:"project_id"`
	Workspace       SessionWorkspace `json:"workspace,omitzero"`
	Provider        string           `json:"provider"`
	Model           *string          `json:"model"`
	RuntimeMode     string           `json:"runtime_mode"`
	InteractionMode string           `json:"interaction_mode"`
	Status          string           `json:"status"`
	ReasoningEffort *string          `json:"reasoning_effort"`
	ServiceTier     *string          `json:"service_tier"`
	ContextWindow   *string          `json:"context_window"`
	AgentPreset     *string          `json:"agent_preset"`
	ProviderCursor  json.RawMessage  `json:"provider_cursor"`
	CreatedAt       uint64           `json:"created_at"`
	UpdatedAt       uint64           `json:"updated_at"`
	LastReplyAt     *uint64          `json:"last_reply_at"`
	Imported        bool             `json:"imported"`
	Archived        bool             `json:"archived"`
	NativeSessionID *string          `json:"native_session_id"`
	Messages        []Message        `json:"messages"`

	TranscriptBlocks []TranscriptBlock `json:"transcript_blocks"`
	Turns            []AgentTurn       `json:"turns"`
	Compaction       *CompactionState  `json:"compaction"`
	QueuedMessages   []QueuedMessage   `json:"queued_messages"`

	// ContextUsage is the provider's latest context occupancy, present on
	// hydrated sessions. Nil means "not known yet".
	ContextUsage       *ContextUsage       `json:"context_usage"`
	RuntimeEventCursor *RuntimeEventCursor `json:"runtime_event_cursor"`

	// Draft marks a session created locally whose first prompt has not been
	// sent; the sidebar hides it until it has a conversation.
	Draft bool `json:"-"`
}

type RuntimeEventCursor struct {
	RuntimeID string `json:"runtime_id"`
	Epoch     string `json:"epoch"`
	Sequence  uint64 `json:"sequence"`
}

// SessionWorkspace preserves the daemon's tagged workspace descriptor.
// Missing workspace data from older sessions means the project checkout.
type SessionWorkspace struct {
	Kind       string  `json:"kind"`
	Path       string  `json:"path,omitempty"`
	Branch     string  `json:"branch,omitempty"`
	BaseBranch *string `json:"baseBranch,omitempty"`
}

// HasStarted reports whether a task has any recorded conversation. A task that
// arrived from the daemon is always started; a local draft is not.
func (s *Session) HasStarted() bool {
	return !s.Draft || len(s.Turns) > 0 || len(s.Messages) > 0
}

// QueuedMessage is a follow-up waiting for the running turn to settle.
type QueuedMessage struct {
	CreatedAt      uint64       `json:"created_at"`
	ID             string       `json:"id"`
	Content        string       `json:"content"`
	DisplayContent *string      `json:"display_content"`
	Attachments    []Attachment `json:"attachments"`
}

// CompactionState is the provider's live context compaction, when any.
type CompactionState struct {
	Status  string  `json:"status"`
	Reason  *string `json:"reason,omitempty"`
	Model   *string `json:"model,omitempty"`
	Error   *string `json:"error,omitempty"`
	Summary *string `json:"summary,omitempty"`
}

// ContextUsage mirrors fintwind-protocol's ContextUsage
// (legacy/rust/crates/fintwind-protocol/src/model.rs): the provider's latest context
// occupancy. Tokens is that call's occupancy; Window is the model's context
// size — nil means "not known yet", and the meter degrades to a bare token
// count. Composition is the provider's estimated breakdown, separate from usage.
type ContextUsage struct {
	Composition     *ContextComposition `json:"composition,omitempty"`
	Tokens          uint64              `json:"tokens"`
	Measured        bool                `json:"measured"`
	Window          *uint64             `json:"window"`
	TotalTokens     *uint64             `json:"total_tokens"`
	CacheRead       *uint64             `json:"cache_read"`
	PromptTokens    *uint64             `json:"prompt_tokens"`
	FirstTokenMs    *uint64             `json:"first_token_ms"`
	FirstTokenTurns *uint64             `json:"first_token_turns"`
}

type ContextComposition struct {
	wire            json.RawMessage
	Version         uint32 `json:"version"`
	Model           string `json:"model"`
	System          uint64 `json:"system"`
	Tools           uint64 `json:"tools"`
	Memory          uint64 `json:"memory"`
	Messages        uint64 `json:"messages"`
	Reasoning       uint64 `json:"reasoning"`
	ToolCalls       uint64 `json:"toolCalls"`
	Files           uint64 `json:"files"`
	ToolResults     uint64 `json:"toolResults"`
	OpaqueReasoning bool   `json:"opaqueReasoning"`
	OpaqueMemory    bool   `json:"opaqueMemory"`
	Incomplete      bool   `json:"incomplete"`
}

// Keep the provider snapshot intact when saving, including fields from newer versions.
func (c *ContextComposition) UnmarshalJSON(data []byte) error {
	type fields ContextComposition
	var decoded fields
	if err := json.Unmarshal(data, &decoded); err != nil {
		return err
	}
	*c = ContextComposition(decoded)
	c.wire = append(json.RawMessage(nil), data...)
	return nil
}

func (c ContextComposition) MarshalJSON() ([]byte, error) {
	if c.wire != nil {
		return c.wire, nil
	}
	type fields ContextComposition
	return json.Marshal(fields(c))
}

// ActiveTurnID is the running turn, when the transcript is still streaming.
func (s *Session) ActiveTurnID() string {
	if len(s.Turns) == 0 {
		return ""
	}
	last := &s.Turns[len(s.Turns)-1]
	if last.Status != "running" {
		return ""
	}
	return last.ID
}

// TurnByID finds one turn of this session.
func (s *Session) TurnByID(turnID string) *AgentTurn {
	for index := range s.Turns {
		if s.Turns[index].ID == turnID {
			return &s.Turns[index]
		}
	}
	return nil
}

// DisplayTitle prefers the user title, then the provider or derived title.
func (s *Session) DisplayTitle() string {
	if s.Title != "" && s.Title != "New task" {
		return s.Title
	}
	if s.AutoTitle != nil && *s.AutoTitle != "" {
		return *s.AutoTitle
	}
	return "New task"
}

// Updated returns the time the session last changed.
func (s *Session) Updated() time.Time {
	if s.UpdatedAt == 0 && (s.LastReplyAt == nil || *s.LastReplyAt == 0) {
		return time.Time{}
	}
	if s.LastReplyAt != nil && *s.LastReplyAt > s.UpdatedAt {
		return time.Unix(int64(*s.LastReplyAt), 0)
	}
	return time.Unix(int64(s.UpdatedAt), 0)
}

// Busy reports whether the agent is still running a turn.
func (s *Session) Busy() bool {
	switch s.Status {
	case "connecting", "working", "waiting":
		return true
	}
	return false
}

// Message is one persisted turn message.
type Message struct {
	ID             string       `json:"id"`
	TurnID         *string      `json:"turn_id"`
	Role           string       `json:"role"`
	Content        string       `json:"content"`
	DisplayContent *string      `json:"display_content"`
	Attachments    []Attachment `json:"attachments"`
	CreatedAt      uint64       `json:"created_at"`
	Streaming      bool         `json:"streaming"`
}

// Text is the user-visible content, without provider-facing mentions.
func (m *Message) Text() string {
	if m.DisplayContent != nil && *m.DisplayContent != "" {
		return *m.DisplayContent
	}
	return m.Content
}

// Attachment is a file sent with a message.
type Attachment struct {
	Path          string  `json:"path"`
	IsDir         bool    `json:"is_dir"`
	BlobReference *string `json:"blob_reference,omitempty"`
	Name          string  `json:"name"`
	Mention       string  `json:"mention"`
	IsImage       bool    `json:"is_image"`
}

// SessionDetail is the payload of a hydrated session.
type SessionDetail struct {
	Session *Session `json:"session"`
}
