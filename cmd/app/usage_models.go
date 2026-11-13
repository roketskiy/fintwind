package main

import (
	"context"
	"time"

	"fintwind-go/internal/daemon"
)

type usageLoadStatus string

const (
	usageIdle         usageLoadStatus = "idle"
	usageLoading      usageLoadStatus = "loading"
	usageReady        usageLoadStatus = "ready"
	usageError        usageLoadStatus = "error"
	usageStaleAfter                   = 60 * time.Second
	usageFetchTimeout                 = 90 * time.Second
	usagePollTimeout                  = 30 * time.Second
)

type usageState struct {
	History       usageHistory
	Subscriptions usageSubscriptions
	Range         string
	SelectedDay   string
	UI            usageUIState
	root          string
	client        *daemon.Client
	identity      daemon.Identity
}

type usageHistory struct {
	Status       usageLoadStatus
	Error        string
	UpdatedAt    time.Time
	Stats        *usageStats
	Views        map[string]*usageViewData
	Days         map[string]*usageViewData
	Heat         []usageHeatDay
	Expired      bool
	Refreshing   bool
	generation   uint64
	attemptedAt  time.Time
	aggregateDay string
	building     bool
	cancel       context.CancelFunc
}

type usageViewData struct {
	Totals    usageTotals
	Daily     []usageDayBucket
	Models    []usageRank
	Providers []usageRank
	Projects  []usageRank
	Start     time.Time
	End       time.Time
}

type usageRank struct {
	ID       string
	Label    string
	Tokens   uint64
	Sessions int
}

type usageHeatDay struct {
	Date     time.Time
	Direct   uint64
	Total    uint64
	Sessions int
}

type usageSubscriptions struct {
	Status      usageLoadStatus
	Error       string
	UpdatedAt   time.Time
	Cards       []usageSubscription
	Expired     bool
	generation  uint64
	attemptedAt time.Time
	pending     int
	cancel      context.CancelFunc
}

type usageSubscription struct {
	ID        string
	Name      string
	Status    usageLoadStatus
	Quota     *usageQuota
	Error     string
	Expired   bool
	UpdatedAt time.Time
}

type usageQuota struct {
	Status     string              `json:"status"`
	Plan       *string             `json:"plan"`
	Service    *string             `json:"service"`
	Account    *string             `json:"account"`
	Windows    []usageQuotaWindow  `json:"windows"`
	Balances   []usageQuotaBalance `json:"balances"`
	MeasuredAt *int64              `json:"measuredAt"`
	RetryAt    *int64              `json:"retryAt"`
}

type usageQuotaWindow struct {
	ID          string   `json:"id"`
	Kind        string   `json:"kind"`
	Model       *string  `json:"model"`
	SpanSeconds *uint64  `json:"spanSeconds"`
	UsedPercent *float64 `json:"usedPercent"`
	Remaining   *string  `json:"remaining"`
	Limit       *string  `json:"limit"`
	ResetsAt    *int64   `json:"resetsAt"`
	Unlimited   bool     `json:"unlimited"`
}

type usageQuotaBalance struct {
	Unit      string `json:"unit"`
	Remaining string `json:"remaining"`
	Unlimited bool   `json:"unlimited"`
}

type usageEntry struct {
	Timestamp        uint64               `json:"timestamp"`
	Model            *string              `json:"model"`
	Directory        *string              `json:"directory"`
	Cost             *float64             `json:"cost"`
	InputTokens      uint64               `json:"inputTokens"`
	OutputTokens     uint64               `json:"outputTokens"`
	ReasoningTokens  uint64               `json:"reasoningTokens"`
	CacheReadTokens  uint64               `json:"cacheReadTokens"`
	CacheWriteTokens uint64               `json:"cacheWriteTokens"`
	SubagentSessions uint32               `json:"subagentSessions"`
	SubagentTokens   uint64               `json:"subagentTokens"`
	SubagentDirect   uint64               `json:"subagentDirect"`
	Days             []usageDayShare      `json:"days"`
	ModelLanes       []usageModelLane     `json:"modelLanes"`
	SubagentModels   []usageSubagentModel `json:"subagentModels"`
}

type usageDayShare struct {
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

type usageModelLane struct {
	Model  string          `json:"model"`
	Total  uint64          `json:"total"`
	Cost   float64         `json:"cost"`
	Tokens [5]uint64       `json:"tokens"`
	Days   []usageDayShare `json:"days"`
}

type usageSubagentModel struct {
	SessionID        string          `json:"sessionId"`
	Model            *string         `json:"model"`
	InputTokens      uint64          `json:"inputTokens"`
	OutputTokens     uint64          `json:"outputTokens"`
	ReasoningTokens  uint64          `json:"reasoningTokens"`
	CacheReadTokens  uint64          `json:"cacheReadTokens"`
	CacheWriteTokens uint64          `json:"cacheWriteTokens"`
	Cost             *float64        `json:"cost"`
	Days             []usageDayShare `json:"days"`
}

type usageStats struct {
	Entries         []usageEntry `json:"entries"`
	Truncated       bool         `json:"truncated"`
	SessionsScanned int          `json:"sessionsScanned"`
	Refreshing      bool         `json:"refreshing"`
	RefreshError    *string      `json:"refreshError"`
}

type usageTotals struct {
	sessions       uint64
	input          uint64
	output         uint64
	reasoning      uint64
	cacheRead      uint64
	cacheWrite     uint64
	activeDays     int
	cost           float64
	costedSessions int
}

func (t usageTotals) total() uint64 {
	return usageAdd(t.input, t.output, t.reasoning, t.cacheRead, t.cacheWrite)
}

func (t usageTotals) generated() uint64 { return usageAdd(t.output, t.reasoning) }

type usageDayBucket struct {
	date     time.Time
	label    string
	input    uint64
	output   uint64
	cache    uint64
	sessions int
}
