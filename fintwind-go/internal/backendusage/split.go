package backendusage

import (
	"context"
	"encoding/json"
	"errors"
	"math"
	"net/url"
	"sort"
	"time"

	"fintwind-go/internal/backendstore"
)

type API interface {
	RoundTrip(context.Context, string, string, any) (json.RawMessage, error)
}
type messageUsage struct {
	ID         string  `json:"id"`
	Timestamp  uint64  `json:"timestamp"`
	Direct     uint64  `json:"direct"`
	Total      uint64  `json:"total"`
	Input      uint64  `json:"input"`
	Output     uint64  `json:"output"`
	Reasoning  uint64  `json:"reasoning"`
	CacheRead  uint64  `json:"cache_read"`
	CacheWrite uint64  `json:"cache_write"`
	Model      *string `json:"model"`
	Cost       float64 `json:"cost"`
}
type sessionSplit struct {
	Days     []DayShare     `json:"days"`
	Hours    []HourShare    `json:"hours"`
	Models   []ModelLane    `json:"models"`
	Messages []messageUsage `json:"messages"`
}

func add(a, b uint64) uint64 {
	if math.MaxUint64-a < b {
		return math.MaxUint64
	}
	return a + b
}
func uintValue(raw json.RawMessage) uint64 {
	var value uint64
	_ = json.Unmarshal(raw, &value)
	return value
}

func optionalUint(raw json.RawMessage) (uint64, bool) {
	var value uint64
	if len(raw) == 0 || string(raw) == "null" || json.Unmarshal(raw, &value) != nil {
		return 0, false
	}
	return value, true
}
func object(raw json.RawMessage) backendstore.Object {
	value, _ := backendstore.Decode(raw)
	return value
}
func reportedModel(raw json.RawMessage) *string {
	model := object(raw)
	provider, id := model.String("providerID"), model.String("modelID")
	if id == "" {
		id = model.String("id")
	}
	if provider == "" || id == "" {
		return nil
	}
	value := provider + "/" + id
	return &value
}
func parseMessage(row backendstore.Object) (messageUsage, bool) {
	if info := row["info"]; len(info) > 0 {
		row = object(info)
	}
	role := row.String("type")
	if role == "" {
		role = row.String("role")
	}
	if role != "" && role != "assistant" {
		return messageUsage{}, false
	}
	timing := object(row["time"])
	millis, validTime := optionalUint(timing["created"])
	stamp := millis / 1000
	if !validTime || stamp > math.MaxInt64 {
		return messageUsage{}, false
	}
	tokens := object(row["tokens"])
	cache := object(tokens["cache"])
	input, out, reason := uintValue(tokens["input"]), uintValue(tokens["output"]), uintValue(tokens["reasoning"])
	m := messageUsage{ID: row.String("id"), Timestamp: stamp, Input: input, Output: add(out, reason), Reasoning: reason, CacheRead: uintValue(cache["read"]), CacheWrite: uintValue(cache["write"]), Model: reportedModel(row["model"])}
	m.Direct = add(input, m.Output)
	m.Total = add(add(m.Direct, m.CacheRead), m.CacheWrite)
	if m.Total == 0 || m.ID == "" {
		return messageUsage{}, false
	}
	_ = json.Unmarshal(row["cost"], &m.Cost)
	if math.IsNaN(m.Cost) || math.IsInf(m.Cost, 0) {
		return messageUsage{}, false
	}
	return m, true
}
func localDay(stamp uint64) string {
	return time.Unix(int64(stamp), 0).In(time.Local).Format("2006-01-02")
}
func addDay(target *DayShare, m messageUsage) {
	target.Direct = add(target.Direct, m.Direct)
	target.Total = add(target.Total, m.Total)
	target.Input = add(target.Input, m.Input)
	target.Output = add(target.Output, m.Output)
	target.CacheRead = add(target.CacheRead, m.CacheRead)
	target.CacheWrite = add(target.CacheWrite, m.CacheWrite)
	target.Cost += m.Cost
}
func addHour(target *HourShare, m messageUsage) {
	target.Input = add(target.Input, m.Input)
	target.Output = add(target.Output, m.Output)
	target.CacheRead = add(target.CacheRead, m.CacheRead)
	target.CacheWrite = add(target.CacheWrite, m.CacheWrite)
	target.Cost += m.Cost
}

type buckets struct {
	days  map[string]int
	hours map[uint64]int
}

func (b *buckets) append(days *[]DayShare, hours *[]HourShare, m messageUsage) {
	if b.days == nil {
		b.days = map[string]int{}
		b.hours = map[uint64]int{}
	}
	day := localDay(m.Timestamp)
	i, ok := b.days[day]
	if !ok {
		i = len(*days)
		b.days[day] = i
		*days = append(*days, DayShare{Timestamp: m.Timestamp})
	}
	addDay(&(*days)[i], m)
	hour := m.Timestamp / 3600
	j, ok := b.hours[hour]
	if !ok {
		j = len(*hours)
		b.hours[hour] = j
		*hours = append(*hours, HourShare{Timestamp: m.Timestamp})
	}
	addHour(&(*hours)[j], m)
}

func scanSplit(ctx context.Context, api API, id string) (sessionSplit, error) {
	ctx, cancel := context.WithTimeout(ctx, 45*time.Second)
	defer cancel()
	split := sessionSplit{Days: []DayShare{}, Hours: []HourShare{}, Models: []ModelLane{}, Messages: []messageUsage{}}
	cursor := ""
	cursors := map[string]bool{}
	seen := map[string]bool{}
	modelIndex := map[string]int{}
	modelBuckets := map[string]*buckets{}
	all := &buckets{}
	bytes := 0
	for page := 0; page < 50; page++ {
		if err := ctx.Err(); err != nil {
			return sessionSplit{}, err
		}
		path := "/api/session/" + url.PathEscape(id) + "/message?limit=200"
		if cursor != "" {
			path += "&cursor=" + url.QueryEscape(cursor)
		}
		get, stop := context.WithTimeout(ctx, 15*time.Second)
		raw, err := api.RoundTrip(get, "GET", path, nil)
		stop()
		if err != nil {
			return sessionSplit{}, err
		}
		bytes += len(raw)
		if bytes > 64<<20 {
			return sessionSplit{}, errors.New("usage message byte budget exceeded")
		}
		var response struct {
			Data   []backendstore.Object `json:"data"`
			Cursor struct {
				Next string `json:"next"`
			} `json:"cursor"`
		}
		if json.Unmarshal(raw, &response) != nil || response.Data == nil || len(response.Data) > 200 {
			return sessionSplit{}, errors.New("invalid usage message listing")
		}
		next := response.Cursor.Next
		if next != "" && (cursors[next] || len(response.Data) == 0 || page == 49) {
			return sessionSplit{}, errors.New("usage message pagination incomplete")
		}
		if next != "" {
			cursors[next] = true
		}
		for _, row := range response.Data {
			m, ok := parseMessage(row)
			if !ok || seen[m.ID] {
				continue
			}
			seen[m.ID] = true
			split.Messages = append(split.Messages, m)
			all.append(&split.Days, &split.Hours, m)
			if m.Model == nil {
				continue
			}
			key := *m.Model
			i, ok := modelIndex[key]
			if !ok {
				i = len(split.Models)
				modelIndex[key] = i
				split.Models = append(split.Models, ModelLane{Model: key, Days: []DayShare{}, Hours: []HourShare{}})
				modelBuckets[key] = &buckets{}
			}
			lane := &split.Models[i]
			lane.Total = add(lane.Total, m.Total)
			lane.Cost += m.Cost
			tokens := [5]uint64{m.Input, m.Output - m.Reasoning, m.Reasoning, m.CacheRead, m.CacheWrite}
			for j := range tokens {
				lane.Tokens[j] = add(lane.Tokens[j], tokens[j])
			}
			lane.Requests = append(lane.Requests, Request{Timestamp: m.Timestamp, Tokens: tokens})
			modelBuckets[key].append(&lane.Days, &lane.Hours, m)
		}
		if next == "" {
			break
		}
		cursor = next
	}
	sort.Slice(split.Days, func(i, j int) bool { return split.Days[i].Timestamp < split.Days[j].Timestamp })
	sort.Slice(split.Hours, func(i, j int) bool { return split.Hours[i].Timestamp < split.Hours[j].Timestamp })
	sort.SliceStable(split.Models, func(i, j int) bool { return split.Models[i].Total > split.Models[j].Total })
	return split, nil
}
