package backendengine

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

type Client struct {
	base           string
	password       string
	directory      string
	http           *http.Client
	generationHTTP *http.Client
	ctx            context.Context
	cancel         context.CancelFunc
	mu             sync.Mutex
	err            error
}

type Event struct {
	Kind      string
	SessionID string
	Payload   json.RawMessage
}

type HTTPError struct{ Status int }

func (e *HTTPError) Error() string { return fmt.Sprintf("OpenCode HTTP status %d", e.Status) }

func Connect(address, password, directory string) (*Client, error) {
	host, _, err := net.SplitHostPort(address)
	if err != nil {
		return nil, errors.New("invalid OpenCode address")
	}
	ip := net.ParseIP(host)
	if ip == nil || !ip.IsLoopback() {
		return nil, errors.New("OpenCode connection must be loopback")
	}
	if password == "" || !filepath.IsAbs(directory) {
		return nil, errors.New("OpenCode password and absolute workspace required")
	}
	ctx, cancel := context.WithCancel(context.Background())
	transport := &http.Transport{Proxy: nil, MaxIdleConns: 16, MaxIdleConnsPerHost: 16, IdleConnTimeout: 30 * time.Second, ResponseHeaderTimeout: 15 * time.Second, DialContext: (&net.Dialer{Timeout: 5 * time.Second}).DialContext}
	ordinary := &http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error { return errors.New("OpenCode redirects refused") }}
	longTransport := transport.Clone()
	longTransport.ResponseHeaderTimeout = 180 * time.Second
	longClient := *ordinary
	longClient.Transport = longTransport
	return &Client{base: "http://" + address, password: password, directory: directory, http: ordinary, generationHTTP: &longClient, ctx: ctx, cancel: cancel}, nil
}

func (c *Client) Close() {
	c.cancel()
	c.http.CloseIdleConnections()
	c.generationHTTP.CloseIdleConnections()
}
func (c *Client) Err() error { c.mu.Lock(); defer c.mu.Unlock(); return c.err }

func (c *Client) request(ctx context.Context, method, path string, body any) (*http.Response, error) {
	var data []byte
	var err error
	if body != nil {
		data, err = json.Marshal(body)
		if err != nil {
			return nil, err
		}
	}
	req, err := http.NewRequestWithContext(ctx, method, c.base+path, bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	req.SetBasicAuth("opencode", c.password)
	req.Header.Set("x-opencode-directory", url.PathEscape(c.directory))
	req.Header.Set("Content-Type", "application/json")
	if generationRequest(method, path) {
		return c.generationHTTP.Do(req)
	}
	return c.http.Do(req)
}

func generationRequest(method, path string) bool {
	if method == "POST" && path == "/api/experimental/generate/text" {
		return true
	}
	if method != "POST" || !strings.HasPrefix(path, "/api/session/") || !strings.HasSuffix(path, "/generate") {
		return false
	}
	id := strings.TrimSuffix(strings.TrimPrefix(path, "/api/session/"), "/generate")
	return id != "" && !strings.ContainsAny(id, "/?#")
}

func (c *Client) call(ctx context.Context, method, path string, body any) (json.RawMessage, error) {
	budget := 30 * time.Second
	if generationRequest(method, path) {
		budget = 120 * time.Second
		if path == "/api/experimental/generate/text" {
			budget = 180 * time.Second
		}
	}
	ctx, cancel := context.WithTimeout(ctx, budget)
	defer cancel()
	stop := context.AfterFunc(c.ctx, cancel)
	defer stop()
	response, err := c.request(ctx, method, path, body)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return nil, &HTTPError{Status: response.StatusCode}
	}
	raw, err := io.ReadAll(io.LimitReader(response.Body, 48<<20+1))
	if err != nil {
		return nil, err
	}
	if len(raw) > 48<<20 {
		return nil, errors.New("OpenCode response exceeds budget")
	}
	if len(raw) == 0 {
		return nil, nil
	}
	if !json.Valid(raw) {
		return nil, errors.New("invalid OpenCode response")
	}
	return raw, nil
}

func (c *Client) CreateSession(ctx context.Context) (string, error) {
	raw, err := c.call(ctx, "POST", "/api/session", map[string]any{"location": map[string]string{"directory": c.directory}})
	if err != nil {
		return "", err
	}
	var value struct {
		Data struct {
			ID string `json:"id"`
		} `json:"data"`
	}
	if json.Unmarshal(raw, &value) != nil || value.Data.ID == "" {
		return "", errors.New("OpenCode did not return a session identity")
	}
	return value.Data.ID, nil
}

type PromptFile struct {
	URI  string `json:"uri"`
	Name string `json:"name"`
}

func (c *Client) Prompt(ctx context.Context, session, inputID, text string, files []PromptFile) (string, error) {
	if session == "" || inputID == "" {
		return "notSent", errors.New("session and durable input identity required")
	}
	skills, err := c.PromptSkills(ctx, text)
	if err != nil {
		return "notSent", err
	}
	body := map[string]any{"text": text, "delivery": "steer", "id": inputID}
	if len(skills) > 0 {
		body["skills"] = skills
	}
	if len(files) > 0 {
		body["files"] = files
	}
	path := "/api/session/" + url.PathEscape(session) + "/prompt"
	if strings.HasPrefix(text, "/") && !strings.ContainsAny(text, "\r\n") {
		name, arguments, _ := strings.Cut(strings.TrimPrefix(text, "/"), " ")
		if name != "" {
			registry, registryErr := c.call(ctx, "GET", "/api/command?location%5Bdirectory%5D="+url.QueryEscape(c.directory), nil)
			var result struct {
				Data []struct {
					Name string `json:"name"`
				} `json:"data"`
			}
			if registryErr == nil && json.Unmarshal(registry, &result) == nil {
				for _, entry := range result.Data {
					if entry.Name == name {
						path = "/api/session/" + url.PathEscape(session) + "/command"
						delete(body, "id")
						body["name"], body["text"] = name, strings.TrimSpace(arguments)
						break
					}
				}
			}
		}
	}
	_, err = c.call(ctx, "POST", path, body)
	if err == nil {
		return "accepted", nil
	}
	var httpErr *HTTPError
	if errors.As(err, &httpErr) && (httpErr.Status == 400 || httpErr.Status == 422) {
		return "rejected", err
	}
	var networkErr *net.OpError
	if errors.As(err, &networkErr) && networkErr.Op == "dial" {
		return "notSent", err
	}
	return "unknown", err
}

func (c *Client) Interrupt(ctx context.Context, session string) error {
	_, err := c.call(ctx, "POST", "/api/session/"+url.PathEscape(session)+"/interrupt", nil)
	return err
}

func (c *Client) Configure(ctx context.Context, session, model, variant, agent string) error {
	path := "/api/session/" + url.PathEscape(session)
	if agent != "" {
		if _, err := c.call(ctx, "POST", path+"/agent", map[string]string{"agent": agent}); err != nil {
			return err
		}
	}
	if model != "" {
		provider, id, ok := strings.Cut(model, "/")
		if !ok || provider == "" || id == "" {
			return errors.New("provider/model identifier required")
		}
		selection := map[string]string{"providerID": provider, "id": id}
		if variant != "" {
			selection["variant"] = variant
		}
		if _, err := c.call(ctx, "POST", path+"/model", map[string]any{"model": selection}); err != nil {
			return err
		}
	}
	return nil
}

func (c *Client) Messages(ctx context.Context, session string) (json.RawMessage, error) {
	return c.call(ctx, "GET", "/api/session/"+url.PathEscape(session)+"/message?limit=200", nil)
}

func (c *Client) Permission(ctx context.Context, session, request, option string) error {
	decision := "reject"
	if option == "allow" || option == "once" || option == "allow_once" {
		decision = "once"
	}
	if option == "always" || option == "allow_always" {
		decision = "always"
	}
	if option == "always" || option == "allow_always" {
		decision = "once"
	}
	_, err := c.call(ctx, "POST", "/api/session/"+url.PathEscape(session)+"/permission/"+url.PathEscape(request)+"/reply", map[string]string{"decision": decision})
	return err
}

func (c *Client) Catalog(ctx context.Context) (json.RawMessage, error) {
	return c.call(ctx, "GET", "/api/model", nil)
}

func (c *Client) Compact(ctx context.Context, session string) error {
	_, err := c.call(ctx, "POST", "/api/session/"+url.PathEscape(session)+"/compact", map[string]any{})
	var httpErr *HTTPError
	if errors.As(err, &httpErr) && httpErr.Status == 409 {
		return nil
	}
	return err
}

func (c *Client) Events(ctx context.Context) (<-chan Event, error) {
	streamCtx, cancel := context.WithCancel(ctx)
	stop := context.AfterFunc(c.ctx, cancel)
	response, err := c.request(streamCtx, "GET", "/api/event", nil)
	if err != nil {
		stop()
		cancel()
		return nil, err
	}
	if response.StatusCode != 200 || !strings.HasPrefix(response.Header.Get("Content-Type"), "text/event-stream") {
		response.Body.Close()
		stop()
		cancel()
		return nil, errors.New("OpenCode event stream rejected")
	}
	events := make(chan Event, 128)
	go func() {
		defer close(events)
		defer response.Body.Close()
		defer stop()
		defer cancel()
		fail := func(err error) {
			c.mu.Lock()
			if c.err == nil {
				c.err = err
			}
			c.mu.Unlock()
		}
		scanner := bufio.NewScanner(response.Body)
		scanner.Buffer(make([]byte, 4096), 1<<20)
		var data strings.Builder
		for scanner.Scan() {
			line := scanner.Text()
			if line != "" {
				if strings.HasPrefix(line, "data:") {
					if data.Len() > 0 {
						data.WriteByte('\n')
					}
					data.WriteString(strings.TrimPrefix(strings.TrimPrefix(line, "data:"), " "))
					if data.Len() > 1<<20 {
						fail(errors.New("OpenCode event exceeds budget"))
						return
					}
				}
				continue
			}
			if data.Len() == 0 {
				continue
			}
			var wire struct {
				Type       string          `json:"type"`
				Data       json.RawMessage `json:"data"`
				Properties json.RawMessage `json:"properties"`
			}
			if err := json.Unmarshal([]byte(data.String()), &wire); err != nil {
				fail(errors.New("invalid OpenCode event"))
				return
			}
			data.Reset()
			payload := wire.Data
			if len(payload) == 0 {
				payload = wire.Properties
			}
			var scope struct {
				ID           string `json:"id"`
				SessionID    string `json:"sessionID"`
				SessionIDAlt string `json:"sessionId"`
				Session      struct {
					ID string `json:"id"`
				} `json:"session"`
				Info struct {
					ID string `json:"id"`
				} `json:"info"`
				Form struct {
					SessionID string `json:"sessionID"`
				} `json:"form"`
			}
			if len(payload) > 0 && string(payload) != "null" {
				if err := json.Unmarshal(payload, &scope); err != nil {
					fail(errors.New("invalid OpenCode event scope"))
					return
				}
			}
			id := scope.SessionID
			if id == "" {
				id = scope.SessionIDAlt
			}
			if id == "" {
				id = scope.Session.ID
			}
			if id == "" {
				id = scope.Info.ID
			}
			if id == "" {
				id = scope.Form.SessionID
			}
			if id == "" && (wire.Type == "session.created" || wire.Type == "session.updated" || wire.Type == "session.deleted") {
				id = scope.ID
			}
			select {
			case events <- Event{Kind: wire.Type, SessionID: id, Payload: payload}:
			case <-streamCtx.Done():
				return
			default:
				fail(errors.New("OpenCode event queue overflow; transcript reconciliation required"))
				return
			}
		}
		if err := scanner.Err(); err != nil && !errors.Is(err, context.Canceled) {
			fail(errors.New("OpenCode event stream interrupted; transcript reconciliation required"))
		} else if streamCtx.Err() == nil {
			fail(errors.New("OpenCode event stream ended; transcript reconciliation required"))
		}
	}()
	return events, nil
}
