package model

// NativeTranscript mirrors fintwind-protocol's NativeTranscript
// (legacy/rust/crates/fintwind-protocol/src/provider_session.rs): a provider session's
// transcript translated into the app's rendering shapes, so the ordinary
// message/block/turn pipeline can consume it. The struct itself is
// camelCase on the wire, but every field is a single word, so the JSON keys
// are exactly "messages", "blocks" and "turns". The nested Message and
// AgentTurn carry snake_case keys, and TranscriptBlock decodes the stored
// tagged content shape through its custom UnmarshalJSON.
type NativeTranscript struct {
	Messages []Message         `json:"messages"`
	Blocks   []TranscriptBlock `json:"blocks"`
	Turns    []AgentTurn       `json:"turns"`
	// DetachedShells is adapter-only discovery evidence, never part of the
	// public transcript wire. A live shell row must still verify ownership.
	DetachedShells []BackgroundWorkItem `json:"-"`
}
