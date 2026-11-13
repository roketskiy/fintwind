package backendengine

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"path/filepath"
	"runtime"
	"strings"
)

type Answer struct {
	QuestionID string   `json:"question_id"`
	Answers    []string `json:"answers"`
}

func (c *Client) RoundTrip(ctx context.Context, method, path string, body any) (json.RawMessage, error) {
	if !strings.HasPrefix(path, "/api/") || strings.ContainsAny(path, "\r\n") {
		return nil, errors.New("invalid OpenCode API path")
	}
	return c.call(ctx, method, path, body)
}

func (c *Client) ResumeSession(ctx context.Context, id string) error {
	if id == "" {
		return errors.New("native session ID required")
	}
	raw, err := c.call(ctx, "GET", "/api/session/"+url.PathEscape(id), nil)
	if err != nil {
		return err
	}
	var response struct {
		Data     json.RawMessage `json:"data"`
		Location struct {
			Directory string `json:"directory"`
		} `json:"location"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return errors.New("invalid OpenCode session response")
	}
	if len(response.Data) > 0 && string(response.Data) != "null" {
		data := response.Data
		response.Location.Directory = ""
		if err := json.Unmarshal(data, &response); err != nil {
			return errors.New("invalid OpenCode session response")
		}
	}
	recorded := strings.TrimSpace(response.Location.Directory)
	if recorded == "" {
		return nil
	}
	normalize := func(path string) string {
		if absolute, err := filepath.Abs(path); err == nil {
			path = absolute
		}
		if resolved, err := filepath.EvalSymlinks(path); err == nil {
			path = resolved
		}
		return filepath.Clean(path)
	}
	actual, expected := normalize(recorded), normalize(c.directory)
	if actual != expected && !(runtime.GOOS == "windows" && strings.EqualFold(actual, expected)) {
		return errors.New("OpenCode session belongs to a different workspace")
	}
	return nil
}

func (c *Client) Answer(ctx context.Context, session, request string, answers []Answer, multi map[string]bool) error {
	if session == "" || request == "" {
		return errors.New("question identity required")
	}
	if !strings.HasPrefix(request, "frm_") {
		values := make([][]string, len(answers))
		for i, answer := range answers {
			values[i] = answer.Answers
		}
		_, err := c.call(ctx, "POST", "/api/question/"+url.PathEscape(request)+"/reply", map[string]any{"answers": values})
		return err
	}
	fields := map[string]any{}
	for _, answer := range answers {
		if answer.QuestionID == "" {
			return errors.New("question field identity required")
		}
		if multi[answer.QuestionID] {
			fields[answer.QuestionID] = answer.Answers
		} else {
			if len(answer.Answers) != 1 {
				return errors.New("single choice requires exactly one answer")
			}
			fields[answer.QuestionID] = answer.Answers[0]
		}
	}
	_, err := c.call(ctx, "POST", "/api/session/"+url.PathEscape(session)+"/form/"+url.PathEscape(request)+"/reply", map[string]any{"answer": fields})
	return err
}

func (c *Client) CancelQuestion(ctx context.Context, session, request string) error {
	if session == "" || request == "" {
		return errors.New("question identity required")
	}
	if strings.HasPrefix(request, "frm_") {
		path := "/api/session/" + url.PathEscape(session) + "/form/" + url.PathEscape(request)
		_, err := c.call(ctx, "POST", path+"/cancel", nil)
		var status *HTTPError
		if errors.As(err, &status) && status.Status == 404 {
			_, err = c.call(ctx, "DELETE", path, nil)
		}
		return err
	}
	_, err := c.call(ctx, "POST", "/api/question/"+url.PathEscape(request)+"/reject", nil)
	return err
}

func (c *Client) Command(ctx context.Context, session, name, text string) error {
	_, err := c.call(ctx, "POST", "/api/session/"+url.PathEscape(session)+"/command", map[string]string{"name": name, "text": text})
	return err
}

func (c *Client) StopWork(ctx context.Context, id string) error {
	_, err := c.call(ctx, "DELETE", "/api/shell/"+url.PathEscape(id)+"?directory="+url.QueryEscape(c.directory), nil)
	return err
}
