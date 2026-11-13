package backendservices

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"strings"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/model"
)

func nativeHistory(ctx context.Context, api API, session string) ([]string, string, error) {
	if session == "" {
		return nil, "", errors.New("native session ID required")
	}
	users := []string{}
	last := ""
	cursor := ""
	seen := map[string]bool{}
	bytes := 0
	for page := 0; page < 10000; page++ {
		path := "/api/session/" + url.PathEscape(session) + "/message?limit=200"
		if cursor != "" {
			path += "&cursor=" + url.QueryEscape(cursor)
		}
		raw, err := api.RoundTrip(ctx, "GET", path, nil)
		if err != nil {
			return nil, "", err
		}
		bytes += len(raw)
		if bytes > 128<<20 {
			return nil, "", errors.New("native history byte budget exceeded")
		}
		var response struct {
			Data   []backendstore.Object `json:"data"`
			Cursor struct {
				Next string `json:"next"`
			} `json:"cursor"`
		}
		if err := json.Unmarshal(raw, &response); err != nil {
			return nil, "", err
		}
		if response.Data == nil {
			return nil, "", errors.New("invalid native history page")
		}
		if page == 0 && len(response.Data) > 0 {
			last = response.Data[0].String("id")
		}
		for _, message := range response.Data {
			if nativeUserTurn(message) {
				id := message.String("id")
				if id == "" {
					return nil, "", errors.New("native user identity missing")
				}
				users = append(users, id)
			}
		}
		if response.Cursor.Next == "" {
			for i, j := 0, len(users)-1; i < j; i, j = i+1, j-1 {
				users[i], users[j] = users[j], users[i]
			}
			return users, last, nil
		}
		if len(response.Data) == 0 || seen[response.Cursor.Next] {
			return nil, "", errors.New("native history cursor stalled")
		}
		cursor = response.Cursor.Next
		seen[cursor] = true
	}
	return nil, "", errors.New("native history page budget exceeded")
}

func nativeUserTurn(message backendstore.Object) bool {
	if message.String("type") != "user" {
		return false
	}
	if strings.TrimSpace(message.String("text")) != "" {
		return true
	}
	var parts []backendstore.Object
	if json.Unmarshal(message["content"], &parts) != nil {
		return false
	}
	for _, part := range parts {
		if part.String("type") == "text" && strings.TrimSpace(part.String("text")) != "" {
			return true
		}
	}
	return false
}

func nativeFork(ctx context.Context, api API, request backendstore.Object) (backendstore.Object, error) {
	session := request.String("session_id")
	if session == "" {
		return nil, errors.New("native session ID required")
	}
	path := "/api/session/" + url.PathEscape(session)
	call := func(method, suffix string, body any) error {
		_, err := api.RoundTrip(ctx, method, path+suffix, body)
		return err
	}
	provider := request.String("provider")
	if provider == "openCodeRedoTurn" {
		if err := call("DELETE", "/revert", nil); err != nil {
			return nil, err
		}
		return nativeCursor(session), nil
	}
	if provider != "openCode" && provider != "openCodeRevert" && provider != "openCodeUndoTurn" {
		return nil, errors.New("unsupported native operation")
	}
	if provider == "openCodeRevert" {
		if err := call("DELETE", "/revert", nil); err != nil {
			return nil, err
		}
	}
	users, last, err := nativeHistory(ctx, api, session)
	if err != nil {
		return nil, err
	}
	if provider == "openCodeUndoTurn" {
		if len(users) == 0 {
			return nil, errors.New("nothing left to undo")
		}
		raw, err := api.RoundTrip(ctx, "GET", path, nil)
		if err != nil {
			return nil, err
		}
		body, err := data(raw)
		if err != nil {
			return nil, err
		}
		info, err := backendstore.Decode(body)
		if err != nil {
			return nil, err
		}
		boundary, _ := backendstore.Decode(info["revert"])
		index := len(users) - 1
		if current := boundary.String("messageID"); current != "" {
			index = -1
			for i, id := range users {
				if id == current {
					index = i - 1
					break
				}
			}
			if index < 0 {
				return nil, errors.New("no earlier native undo boundary")
			}
		}
		if err := call("POST", "/revert/stage", map[string]string{"messageID": users[index]}); err != nil {
			return nil, err
		}
		return nativeCursor(session), nil
	}
	retained, err := request.Int("turn_count")
	if err != nil || retained < 0 || retained > int64(len(users)) {
		return nil, errors.New("retained count exceeds native user history")
	}
	before := ""
	if retained < int64(len(users)) {
		before = users[retained]
	}
	if provider == "openCodeRevert" {
		if before != "" {
			if err := call("POST", "/revert/stage", map[string]string{"messageID": before}); err != nil {
				return nil, err
			}
			if err := call("POST", "/revert/commit", map[string]any{}); err != nil {
				return nil, err
			}
		}
		return nativeCursor(session), nil
	}
	return forkNativeWindow(ctx, api, session, before, last)
}

func forkNativeWindow(ctx context.Context, api API, session, before, last string) (backendstore.Object, error) {
	if before == "" && last == "" {
		return nativeCursor(session), nil
	}
	body := map[string]any{}
	if before != "" {
		body["before"] = before
	}
	raw, err := api.RoundTrip(ctx, "POST", "/api/session/"+url.PathEscape(session)+"/fork", body)
	if err != nil {
		return nil, err
	}
	value, err := data(raw)
	if err != nil {
		return nil, err
	}
	info, err := backendstore.Decode(value)
	if err != nil {
		return nil, err
	}
	forkID := info.String("id")
	if forkID == "" {
		return nil, errors.New("fork response missing native session ID")
	}
	return nativeCursor(forkID), nil
}

// ForkRuntime uses the actor's bound engine. It copies native history only;
// it never truncates or persists the source task.
func ForkRuntime(ctx context.Context, api API, session string, remove int64) (backendstore.Object, error) {
	if remove < 0 {
		return nil, errors.New("non-negative fork turn removal required")
	}
	users, last, err := nativeHistory(ctx, api, session)
	if err != nil {
		return nil, err
	}
	if remove > int64(len(users)) {
		return nil, errors.New("fork removal exceeds native user history")
	}
	retained := len(users) - int(remove)
	before := ""
	if retained < len(users) {
		before = users[retained]
	}
	result, err := forkNativeWindow(ctx, api, session, before, last)
	if err != nil {
		return nil, err
	}
	body, err := backendstore.Decode(result["result"])
	if err != nil {
		return nil, err
	}
	return backendstore.Reply("cursor", backendstore.Fields(map[string]any{"cursor": json.RawMessage(body["cursor"])})), nil
}

func nativeCursor(id string) backendstore.Object {
	return backendstore.Reply("providerSessionForked", backendstore.Fields(map[string]any{"result": map[string]any{"cursor": map[string]string{"provider": "openCode", "sessionId": id}}}))
}

func fetchNativeTranscript(ctx context.Context, api API, session string) (backendstore.Object, error) {
	reader, ok := api.(interface {
		Transcript(context.Context, string) (model.NativeTranscript, error)
	})
	if !ok {
		return nil, errors.New("native transcript adapter unavailable")
	}
	transcript, err := reader.Transcript(ctx, session)
	if err != nil {
		return nil, err
	}
	blocks := make([]json.RawMessage, 0, len(transcript.Blocks))
	for _, block := range transcript.Blocks {
		blocks = append(blocks, block.Raw)
	}
	return backendstore.Reply("nativeTranscript", backendstore.Fields(map[string]any{"transcript": map[string]any{"messages": transcript.Messages, "blocks": blocks, "turns": transcript.Turns}})), nil
}
