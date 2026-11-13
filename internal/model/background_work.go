package model

import "encoding/json"

// Background work uses the Rust desktop wire schema. The transcript wrapper
// deliberately uses transcriptBlocks, not NativeTranscript's blocks field.
type BackgroundWorkKey struct {
	Kind       string `json:"kind"`
	ProviderID string `json:"providerId"`
}
type BackgroundWorkItem struct {
	Key               BackgroundWorkKey `json:"key"`
	Title             string            `json:"title"`
	Detail            *string           `json:"detail"`
	Command           *string           `json:"command"`
	Cwd               *string           `json:"cwd"`
	Output            *string           `json:"output"`
	OutputTruncated   bool              `json:"outputTruncated"`
	StartedAtMs       uint64            `json:"startedAtMs"`
	UpdatedAtMs       uint64            `json:"updatedAtMs"`
	DurationMs        *uint64           `json:"durationMs"`
	ExitCode          *int32            `json:"exitCode"`
	Background        bool              `json:"background"`
	CanStop           bool              `json:"canStop"`
	ControlID         *string           `json:"controlId"`
	OriginActivityIDs []string          `json:"originActivityIds"`
	Role              *string           `json:"role"`
	Model             *string           `json:"model"`
	ParentID          *string           `json:"parentId"`
	ContextTokens     *uint64           `json:"contextTokens,omitempty"`
	Status            string            `json:"status"`
}

func (i BackgroundWorkItem) Live() bool {
	return i.Status == "starting" || i.Status == "running" || i.Status == "monitoring" || i.Status == "stopping"
}

type BackgroundWorkTranscript struct {
	Messages         []Message         `json:"messages"`
	TranscriptBlocks []json.RawMessage `json:"transcriptBlocks"`
	Turns            []AgentTurn       `json:"turns"`
}
type BackgroundWorkSnapshot struct {
	Item       BackgroundWorkItem       `json:"item"`
	Transcript BackgroundWorkTranscript `json:"transcript"`
}

func BackgroundTranscript(native NativeTranscript) BackgroundWorkTranscript {
	blocks := make([]json.RawMessage, 0, len(native.Blocks))
	for _, block := range native.Blocks {
		if len(block.Raw) > 0 {
			blocks = append(blocks, append(json.RawMessage(nil), block.Raw...))
		}
	}
	return BackgroundWorkTranscript{Messages: native.Messages, TranscriptBlocks: blocks, Turns: native.Turns}
}
