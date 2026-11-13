package backendengine

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"strings"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"

	"github.com/google/uuid"
)

func (c *Client) Transcript(ctx context.Context, session string) (model.NativeTranscript, error) {
	result := model.NativeTranscript{Messages: []model.Message{}, Blocks: []model.TranscriptBlock{}, Turns: []model.AgentTurn{}}
	rows := []backendstore.Object{}
	seen := map[string]bool{}
	cursor := ""
	bytes := 0
	for page := 0; page < 10000; page++ {
		path := "/api/session/" + url.PathEscape(session) + "/message?limit=200"
		if cursor != "" {
			path += "&cursor=" + url.QueryEscape(cursor)
		}
		raw, err := c.call(ctx, "GET", path, nil)
		if err != nil {
			return result, err
		}
		bytes += len(raw)
		if bytes > 128<<20 {
			return result, errors.New("native transcript exceeds byte budget")
		}
		var response struct {
			Data   []backendstore.Object `json:"data"`
			Cursor struct {
				Next string `json:"next"`
			} `json:"cursor"`
		}
		if err := json.Unmarshal(raw, &response); err != nil {
			return result, err
		}
		if response.Data == nil {
			return result, errors.New("invalid native transcript page")
		}
		rows = append(rows, response.Data...)
		if response.Cursor.Next == "" {
			break
		}
		if len(response.Data) == 0 || seen[response.Cursor.Next] {
			return result, errors.New("native transcript cursor did not advance")
		}
		seen[response.Cursor.Next] = true
		cursor = response.Cursor.Next
		if page == 9999 {
			return result, errors.New("native transcript page limit reached")
		}
	}
	for left, right := 0, len(rows)-1; left < right; left, right = left+1, right-1 {
		rows[left], rows[right] = rows[right], rows[left]
	}
	turnID := ""
	for _, row := range rows {
		role := row.String("type")
		if role == "" {
			role = row.String("role")
		}
		if role != "user" && role != "assistant" {
			continue
		}
		var timing struct {
			Created   uint64 `json:"created"`
			Completed uint64 `json:"completed"`
		}
		_ = json.Unmarshal(row["time"], &timing)
		if role == "user" || turnID == "" {
			turnID = uuid.NewSHA1(uuid.NameSpaceURL, []byte(session+":"+row.String("id"))).String()
			result.Turns = append(result.Turns, model.AgentTurn{ID: turnID, Status: "completed", TurnCount: len(result.Turns) + 1, StartedAt: timing.Created / 1000})
		}
		parts := []backendstore.Object{}
		if raw := row["content"]; len(raw) > 0 && string(raw) != "null" {
			if err := json.Unmarshal(raw, &parts); err != nil {
				return result, errors.New("invalid native content parts")
			}
		}
		if len(parts) == 0 && row.String("text") != "" {
			parts = append(parts, backendstore.Fields(map[string]any{"type": "text", "text": row.String("text")}))
		}
		var text strings.Builder
		flush := func() {
			if text.Len() == 0 {
				return
			}
			id := uuid.NewSHA1(uuid.NameSpaceURL, []byte(session+":"+row.String("id")+":"+string(rune(len(result.Messages))))).String()
			tid := turnID
			result.Messages = append(result.Messages, model.Message{ID: id, TurnID: &tid, Role: role, Content: text.String(), CreatedAt: timing.Created / 1000, Attachments: []model.Attachment{}})
			text.Reset()
		}
		for _, part := range parts {
			switch part.String("type") {
			case "text":
				text.WriteString(part.String("text"))
			case "reasoning":
				flush()
				tid := turnID
				activity := model.ActivityItem{ID: uuid.NewSHA1(uuid.NameSpaceURL, []byte(session+row.String("id")+part.String("text"))).String(), Kind: "reasoning", Title: "Reasoning", Complete: true, Reasoning: &model.ReasoningBlock{Content: part.String("text"), StartedAtMs: timing.Created, FinishedAtMs: timing.Completed}}
				block := map[string]any{"after_message": len(result.Messages), "turn_id": tid, "content": map[string]any{"kind": "activities", "data": []model.ActivityItem{activity}}}
				raw, _ := json.Marshal(block)
				var translated model.TranscriptBlock
				if err := json.Unmarshal(raw, &translated); err != nil {
					return result, err
				}
				result.Blocks = append(result.Blocks, translated)
			case "tool":
				flush()
				state, err := backendstore.Decode(part["state"])
				if err != nil {
					return result, err
				}
				detail := state.String("output")
				source := part.String("id")
				activity := model.ActivityItem{ID: uuid.NewSHA1(uuid.NameSpaceURL, []byte(session+row.String("id")+source+part.String("tool"))).String(), SourceID: &source, Kind: model.ToolKind(part.String("tool")), Title: state.String("title"), Output: &detail, Complete: state.String("status") == "completed" || state.String("status") == "error" || state.String("status") == "failed", Failed: state.String("status") == "error" || state.String("status") == "failed"}
				if raw := state["input"]; len(raw) > 0 && string(raw) != "null" {
					value := string(raw)
					activity.Arguments = &value
				}
				if raw := state["error"]; activity.Failed && len(raw) > 0 && string(raw) != "null" {
					var text string
					if json.Unmarshal(raw, &text) != nil {
						var errorBody struct {
							Message string `json:"message"`
						}
						_ = json.Unmarshal(raw, &errorBody)
						text = errorBody.Message
						if text == "" {
							text = string(raw)
						}
					}
					activity.Output = &text
				}
				if activity.Title == "" {
					activity.Title = part.String("tool")
				}
				output := state["output"]
				if activity.Failed && len(state["error"]) > 0 {
					output = state["error"]
				}
				metadataSource, _ := json.Marshal(part)
				model.ApplyToolMetadata(&activity, state["input"], output, metadataSource)
				if metadata, e := backendstore.Decode(state["metadata"]); e == nil {
					if shellID := metadata.String("shellID"); shellID != "" && len(result.DetachedShells) < 256 {
						input, _ := backendstore.Decode(state["input"])
						command := input.String("command")
						if command == "" {
							command = input.String("cmd")
						}
						id, parent := shellID, session
						result.DetachedShells = append(result.DetachedShells, model.BackgroundWorkItem{Key: model.BackgroundWorkKey{Kind: "process", ProviderID: id}, Title: command, Command: &command, ParentID: &parent, ControlID: &id, Background: true, CanStop: true, Status: "running", OriginActivityIDs: []string{source}})
					}
				}
				raw, _ := json.Marshal(map[string]any{"after_message": len(result.Messages), "turn_id": turnID, "content": map[string]any{"kind": "activities", "data": []model.ActivityItem{activity}}})
				var translated model.TranscriptBlock
				if err := json.Unmarshal(raw, &translated); err != nil {
					return result, err
				}
				result.Blocks = append(result.Blocks, translated)
			}
		}
		flush()
		if timing.Completed > 0 && len(result.Turns) > 0 {
			completed := timing.Completed / 1000
			result.Turns[len(result.Turns)-1].CompletedAt = &completed
		}
	}
	return result, nil
}
