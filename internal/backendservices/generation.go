package backendservices

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/backendstore"
	"github.com/google/uuid"
)

const generationInstructions = "The user is asking a quick side question about the conversation so far. Answer directly and concisely in markdown from what you already know. Do not call any tools and do not take any actions."

type generations struct {
	mu        sync.Mutex
	active    map[string]context.CancelFunc
	cancelled []string
}

func (g *generations) cancel(id string) error {
	if uuid.Validate(id) != nil {
		return errors.New("valid generation identity required")
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if cancel := g.active[id]; cancel != nil {
		cancel()
		return nil
	}
	for _, old := range g.cancelled {
		if old == id {
			return nil
		}
	}
	if len(g.cancelled) == 64 {
		g.cancelled = g.cancelled[1:]
	}
	g.cancelled = append(g.cancelled, id)
	return nil
}

func (g *generations) begin(ctx context.Context, id string) (context.Context, func(), error) {
	if uuid.Validate(id) != nil {
		return nil, nil, errors.New("valid generation identity required")
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	for _, old := range g.cancelled {
		if old == id {
			return nil, nil, errors.New("session generation cancelled")
		}
	}
	if g.active == nil {
		g.active = map[string]context.CancelFunc{}
	}
	if g.active[id] != nil || len(g.active) >= 64 {
		return nil, nil, errors.New("side generation already running or at capacity")
	}
	child, cancel := context.WithTimeout(ctx, 120*time.Second)
	g.active[id] = cancel
	return child, func() { cancel(); g.mu.Lock(); delete(g.active, id); g.mu.Unlock() }, nil
}

func (s *Service) generate(ctx context.Context, c backendstore.Object) (backendstore.Object, error) {
	question := strings.TrimSpace(c.String("question"))
	if question == "" {
		return nil, errors.New("side question required")
	}
	session := c.String("sessionId")
	if session == "" {
		return nil, errors.New("native session identity required")
	}
	ctx, finish, err := s.generations.begin(ctx, c.String("generationId"))
	if err != nil {
		return nil, err
	}
	defer finish()
	api, err := s.acquire(ctx, c)
	if err != nil {
		return nil, err
	}
	path := "/api/session/" + url.PathEscape(session)
	if raw := c["model"]; len(raw) > 0 && string(raw) != "null" {
		var expected struct {
			Model   string  `json:"model"`
			Variant *string `json:"variant"`
		}
		if err := json.Unmarshal(raw, &expected); err != nil {
			return nil, err
		}
		guard, cancel := context.WithTimeout(ctx, 10*time.Second)
		snapshot, err := api.RoundTrip(guard, "GET", path, nil)
		cancel()
		if err != nil {
			return nil, err
		}
		body, err := data(snapshot)
		if err != nil {
			return nil, err
		}
		row, err := backendstore.Decode(body)
		if err != nil {
			return nil, err
		}
		var native struct {
			Provider string  `json:"providerID"`
			ID       string  `json:"id"`
			Variant  *string `json:"variant"`
		}
		if len(row["model"]) > 0 {
			if err := json.Unmarshal(row["model"], &native); err != nil {
				return nil, err
			}
		}
		provider, model, ok := strings.Cut(expected.Model, "/")
		variantMatches := expected.Variant == nil && native.Variant == nil || expected.Variant != nil && native.Variant != nil && *expected.Variant == *native.Variant
		if !ok || provider == "" || model == "" || native.Provider != provider || native.ID != model || !variantMatches {
			return backendstore.Reply("sessionGenerationModelNotSynced", nil), nil
		}
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	raw, err := api.RoundTrip(ctx, "POST", path+"/generate", map[string]string{"prompt": generationInstructions + "\n\n" + question})
	if err != nil {
		return nil, err
	}
	var response struct {
		Data struct {
			Text *string `json:"text"`
		} `json:"data"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return nil, err
	}
	if response.Data.Text == nil {
		return nil, errors.New("invalid side generation response")
	}
	text := strings.TrimSpace(*response.Data.Text)
	if text == "" {
		return nil, errors.New("empty side generation answer")
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return backendstore.Reply("sessionTextGenerated", backendstore.Fields(map[string]any{"text": text})), nil
}
