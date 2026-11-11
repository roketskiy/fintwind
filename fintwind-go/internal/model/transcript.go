package model

import "encoding/json"

// ActivityKind values in the daemon's camelCase encoding.
const (
	KindReasoning  = "reasoning"
	KindCommand    = "command"
	KindFileChange = "fileChange"
	KindFileRead   = "fileRead"
	KindFileSearch = "fileSearch"
	KindFileList   = "fileList"
	KindSearch     = "search"
	KindPlan       = "plan"
	KindTool       = "tool"
)

// ReasoningBlock is one thought attached to an activity.
type ReasoningBlock struct {
	Content      string `json:"content"`
	StartedAtMs  uint64 `json:"started_at_ms"`
	FinishedAtMs uint64 `json:"finished_at_ms"`
}

// ActivityFileChange is one edited file carried by an activity.
type ActivityFileChange struct {
	Path      string  `json:"path"`
	Additions *uint64 `json:"additions"`
	Deletions *uint64 `json:"deletions"`
	Status    *string `json:"status"`
	Diff      *string `json:"diff"`
}

// DisplayName is the file name without its directory.
func (c ActivityFileChange) DisplayName() string {
	return pathName(c.Path)
}

// ActivityItem is one non-message transcript entry: a thought or a tool call.
type ActivityItem struct {
	ID                 string               `json:"id"`
	SourceID           *string              `json:"source_id"`
	Kind               string               `json:"kind"`
	Title              string               `json:"title"`
	Detail             *string              `json:"detail"`
	Arguments          *string              `json:"arguments"`
	Output             *string              `json:"output"`
	ImageURLs          []string             `json:"image_urls"`
	Failed             bool                 `json:"failed"`
	Complete           bool                 `json:"complete"`
	FileChanges        []ActivityFileChange `json:"file_changes"`
	DisplayTarget      *string              `json:"display_target"`
	DisplayDescription *string              `json:"display_description"`
	Reasoning          *ReasoningBlock      `json:"reasoning"`
}

// TranscriptBlock anchors a run of activities after this many messages.
type TranscriptBlock struct {
	Raw          json.RawMessage `json:"-"`
	ContentKind  string          `json:"-"`
	AfterMessage int             `json:"after_message"`
	TurnID       *string         `json:"turn_id"`
	Activities   []ActivityItem  `json:"activities"`
}

// UnmarshalJSON reads the stored tagged content: activities, or the legacy
// single reasoning block.
func (b *TranscriptBlock) UnmarshalJSON(data []byte) error {
	var raw struct {
		AfterMessage int     `json:"after_message"`
		TurnID       *string `json:"turn_id"`
		Content      struct {
			Kind string          `json:"kind"`
			Data json.RawMessage `json:"data"`
		} `json:"content"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	b.AfterMessage = raw.AfterMessage
	b.Raw = append(json.RawMessage(nil), data...)
	b.ContentKind = raw.Content.Kind
	b.TurnID = raw.TurnID
	b.Activities = nil
	switch raw.Content.Kind {
	case "activities":
		if len(raw.Content.Data) > 0 {
			if err := json.Unmarshal(raw.Content.Data, &b.Activities); err != nil {
				return err
			}
		}
	case "reasoning":
		var reasoning ReasoningBlock
		if err := json.Unmarshal(raw.Content.Data, &reasoning); err != nil {
			return err
		}
		b.Activities = []ActivityItem{{
			Kind:      KindReasoning,
			Title:     "Reasoning",
			Complete:  true,
			Reasoning: &reasoning,
		}}
	}
	return nil
}

// AgentTurn is one submitted prompt and the work it started.
type AgentTurn struct {
	Submission          json.RawMessage `json:"submission,omitempty"`
	ProviderTurnStarted bool            `json:"provider_turn_started"`
	ProviderPrompt      *string         `json:"provider_prompt,omitempty"`
	ID                  string          `json:"id"`
	Status              string          `json:"status"`
	TurnCount           int             `json:"turn_count"`
	StartedAt           uint64          `json:"started_at"`
	CompletedAt         *uint64         `json:"completed_at"`
	Checkpoint          *Checkpoint     `json:"checkpoint"`
	Stats               *TurnStats      `json:"stats"`
}

// TurnStats is the provider-reported usage of one settled turn.
type TurnStats struct {
	Model          *string `json:"model"`
	Agent          *string `json:"agent"`
	OutputTokens   uint64  `json:"output_tokens"`
	StreamMs       uint64  `json:"stream_ms"`
	StreamVerified bool    `json:"stream_verified"`
}

// Checkpoint captures the workspace state before and after a turn.
type Checkpoint struct {
	Status string           `json:"status"`
	Files  []CheckpointFile `json:"files"`
}

// CheckpointFile is one file carried by a turn checkpoint.
type CheckpointFile struct {
	Path      string `json:"path"`
	Additions uint64 `json:"additions"`
	Deletions uint64 `json:"deletions"`
}

// Running reports whether this turn is still in flight.
func (t *AgentTurn) Running() bool {
	return t.Status == "running"
}

// pathName returns the last path segment of a slash or backslash path.
func pathName(path string) string {
	last := -1
	for i, r := range path {
		if r == '/' || r == '\\' {
			last = i
		}
	}
	name := path[last+1:]
	if name == "" {
		return path
	}
	return name
}
