// Package backendusage owns the daemon's source-scoped incremental usage index.
package backendusage

import "encoding/json"

type Stats struct {
	Entries         []Entry `json:"entries"`
	Truncated       bool    `json:"truncated"`
	SessionsScanned int     `json:"sessionsScanned"`
	Refreshing      bool    `json:"refreshing"`
	RefreshError    *string `json:"refreshError"`
}

type Entry struct {
	Timestamp        uint64          `json:"timestamp"`
	Model            *string         `json:"model,omitempty"`
	Directory        *string         `json:"directory,omitempty"`
	Cost             *float64        `json:"cost,omitempty"`
	Input            uint64          `json:"inputTokens"`
	Output           uint64          `json:"outputTokens"`
	Reasoning        uint64          `json:"reasoningTokens"`
	CacheRead        uint64          `json:"cacheReadTokens"`
	CacheWrite       uint64          `json:"cacheWriteTokens"`
	SubagentSessions uint32          `json:"subagentSessions"`
	SubagentTokens   uint64          `json:"subagentTokens"`
	SubagentDirect   uint64          `json:"subagentDirect"`
	Days             []DayShare      `json:"days,omitempty"`
	Hours            []HourShare     `json:"hours,omitempty"`
	ModelLanes       []ModelLane     `json:"modelLanes,omitempty"`
	SubagentModels   []SubagentModel `json:"subagentModels,omitempty"`
}

// Rust's Option<Vec<_>> distinguishes no verified split from an explicitly
// verified empty split. Preserve that distinction on the wire.
func (e Entry) MarshalJSON() ([]byte, error) {
	type alias Entry
	var days *[]DayShare
	if e.Days != nil {
		days = &e.Days
	}
	return json.Marshal(struct {
		alias
		Days *[]DayShare `json:"days,omitempty"`
	}{alias: alias(e), Days: days})
}

type DayShare struct {
	Timestamp  uint64  `json:"timestamp"`
	Subagent   bool    `json:"subagent"`
	Direct     uint64  `json:"direct"`
	Total      uint64  `json:"total"`
	Input      uint64  `json:"input"`
	Output     uint64  `json:"output"`
	CacheRead  uint64  `json:"cacheRead"`
	CacheWrite uint64  `json:"cacheWrite"`
	Cost       float64 `json:"cost"`
}
type HourShare struct {
	Timestamp  uint64  `json:"timestamp"`
	Estimated  bool    `json:"estimated"`
	Input      uint64  `json:"input"`
	Output     uint64  `json:"output"`
	CacheRead  uint64  `json:"cacheRead"`
	CacheWrite uint64  `json:"cacheWrite"`
	Cost       float64 `json:"cost"`
}
type Request struct {
	Timestamp uint64    `json:"timestamp"`
	Tokens    [5]uint64 `json:"tokens"`
}
type ModelLane struct {
	Model    string      `json:"model"`
	Total    uint64      `json:"total"`
	Cost     float64     `json:"cost"`
	Tokens   [5]uint64   `json:"tokens"`
	Days     []DayShare  `json:"days"`
	Hours    []HourShare `json:"hours"`
	Requests []Request   `json:"requests,omitempty"`
}
type SubagentModel struct {
	SessionID  string      `json:"sessionId"`
	Model      *string     `json:"model"`
	Input      uint64      `json:"inputTokens"`
	Output     uint64      `json:"outputTokens"`
	Reasoning  uint64      `json:"reasoningTokens"`
	CacheRead  uint64      `json:"cacheReadTokens"`
	CacheWrite uint64      `json:"cacheWriteTokens"`
	Cost       *float64    `json:"cost"`
	Days       []DayShare  `json:"days"`
	Hours      []HourShare `json:"hours"`
	Requests   []Request   `json:"requests,omitempty"`
}
