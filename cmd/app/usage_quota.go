package main

import (
	"context"
	"encoding/json"
	"math"
	"strings"
	"sync"
	"time"
	"unicode"

	"fintwind-go/internal/daemon"
)

type usageIntegration struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	Connected bool   `json:"connected"`
	NeedsAuth bool   `json:"needsAuth"`
}

// This is the protocol's exact quota catalog in the native board's display order.
func usageSubscriptionOrder() []string {
	return []string{
		"anthropic", "openai", "github-copilot", "google", "opencode", "opencode-go",
		"zhipuai-coding-plan", "zai-coding-plan", "kimi-code-plan-cn", "kimi-code-plan-global",
		"minimax-coding-plan", "minimax-cn-coding-plan", "commandcode", "ollama-cloud",
		"zhipuai", "zai", "minimax", "minimax-cn", "openrouter", "siliconflow", "siliconflow-cn",
		"moonshotai", "moonshotai-cn", "deepseek", "stepfun", "stepfun-ai", "stepfun-ai-step-plan", "stepfun-step-plan",
	}
}

func (a *app) refreshUsageSubscriptions(force bool) {
	if !a.syncUsageRoute() {
		return
	}
	s := &a.usage.Subscriptions
	if s.Status == usageLoading || (!force && !s.attemptedAt.IsZero() && time.Since(s.attemptedAt) < usageStaleAfter) {
		return
	}
	if s.cancel != nil {
		s.cancel()
	}
	ctx, cancel := context.WithCancel(a.lifetime)
	s.cancel = cancel
	s.generation++
	gen := s.generation
	s.Status, s.Error, s.Expired = usageLoading, "", len(s.Cards) > 0
	s.attemptedAt = time.Now()
	old := make(map[string]usageSubscription, len(s.Cards))
	for i := range s.Cards {
		s.Cards[i].Expired = true
		old[s.Cards[i].ID] = s.Cards[i]
	}
	root, client, identity := a.usage.root, a.usage.client, a.usage.identity
	go func() {
		defer cancel()
		setupCtx, setupCancel := context.WithTimeout(ctx, usageFetchTimeout)
		binary := configuredOpencodeBinary(setupCtx, client)
		payload, err := client.Request(setupCtx, map[string]any{
			"type": "fetchIntegrations", "binary": binary, "directory": root,
		})
		setupCancel()
		var body struct {
			Type         string             `json:"type"`
			Integrations []usageIntegration `json:"integrations"`
		}
		if err != nil || json.Unmarshal(payload, &body) != nil || body.Type != "integrations" || body.Integrations == nil {
			a.post(func() {
				if !a.usageRouteMatches(root, client, identity) || a.usage.Subscriptions.generation != gen {
					return
				}
				s := &a.usage.Subscriptions
				s.Status, s.Error, s.Expired = usageError, tr("Could not load connected services"), len(s.Cards) > 0
				s.attemptedAt, s.cancel = time.Now(), nil
			})
			return
		}
		integrations := make(map[string]usageIntegration, len(body.Integrations))
		for _, integration := range body.Integrations {
			integrations[integration.ID] = integration
		}
		var cards []usageSubscription
		var jobs []string
		for _, id := range usageSubscriptionOrder() {
			integration, found := integrations[id]
			if !found || (!integration.Connected && !integration.NeedsAuth) {
				continue
			}
			card := old[id]
			card.ID, card.Name = id, usagePlainLabel(integration.Name)
			if card.Name == "" {
				card.Name = id
			}
			card.Error, card.Expired = "", card.Quota != nil
			if integration.NeedsAuth {
				card.Status, card.Error, card.Expired = usageError, tr("Authorization required"), false
				card.Quota = &usageQuota{Status: "needs_auth"}
			} else if integration.Connected {
				card.Status = usageLoading
				jobs = append(jobs, id)
			}
			cards = append(cards, card)
		}
		a.post(func() {
			if !a.usageRouteMatches(root, client, identity) || a.usage.Subscriptions.generation != gen {
				return
			}
			s := &a.usage.Subscriptions
			s.Cards, s.pending, s.Expired = cards, len(jobs), false
			if len(jobs) == 0 {
				s.Status, s.UpdatedAt, s.attemptedAt, s.cancel = usageReady, time.Now(), time.Now(), nil
			}
		})
		queue := make(chan string, len(jobs))
		for _, id := range jobs {
			queue <- id
		}
		close(queue)
		var workers sync.WaitGroup
		for i := 0; i < min(4, len(jobs)); i++ {
			workers.Add(1)
			go func() {
				defer workers.Done()
				for id := range queue {
					if ctx.Err() != nil {
						return
					}
					requestCtx, requestCancel := context.WithTimeout(ctx, usageFetchTimeout)
					payload, err := client.Request(requestCtx, map[string]any{
						"type": "fetchProviderQuota", "binary": binary, "directory": root, "providerId": id, "force": force,
					})
					requestCancel()
					quota := usageParseQuota(payload)
					if err != nil {
						quota = nil
					}
					a.publishUsageQuota(root, client, identity, gen, id, quota)
				}
			}()
		}
		workers.Wait()
	}()
}

func (a *app) publishUsageQuota(root string, client *daemon.Client, identity daemon.Identity, gen uint64, id string, quota *usageQuota) {
	a.post(func() {
		if !a.usageRouteMatches(root, client, identity) || a.usage.Subscriptions.generation != gen {
			return
		}
		s := &a.usage.Subscriptions
		for i := range s.Cards {
			card := &s.Cards[i]
			if card.ID != id {
				continue
			}
			if quota == nil {
				card.Status, card.Error = usageError, tr("Could not update quota")
				card.Expired = card.Quota != nil
			} else {
				card.Quota, card.Error = quota, usageQuotaError(quota.Status)
				card.Status, card.Expired = usageReady, quota.Status != "ready"
				if card.Error != "" {
					card.Status = usageError
				}
				if quota.Status == "ready" {
					card.UpdatedAt = time.Now()
				}
			}
			break
		}
		s.pending = max(0, s.pending-1)
		if s.pending == 0 {
			s.Status, s.Error, s.Expired = usageReady, "", false
			for _, card := range s.Cards {
				if card.Status == usageError {
					s.Status, s.Error = usageError, tr("Some quotas could not be updated")
				}
				s.Expired = s.Expired || card.Expired
			}
			s.UpdatedAt, s.attemptedAt, s.cancel = time.Now(), time.Now(), nil
		}
	})
}

func usageParseQuota(payload json.RawMessage) *usageQuota {
	var body struct {
		Type  string      `json:"type"`
		Quota *usageQuota `json:"quota"`
	}
	if json.Unmarshal(payload, &body) != nil || body.Type != "providerQuota" || body.Quota == nil {
		return nil
	}
	q := body.Quota
	switch q.Status {
	case "ready", "unsupported", "no_credential", "needs_auth", "access_denied", "rate_limited", "unavailable", "no_data":
	default:
		return nil
	}
	q.Plan, q.Service, q.Account = usagePlainOptional(q.Plan), usagePlainOptional(q.Service), usagePlainOptional(q.Account)
	for i := range q.Windows {
		window := &q.Windows[i]
		window.ID = usagePlainLabel(window.ID)
		window.Model, window.Remaining, window.Limit = usagePlainOptional(window.Model), usagePlainOptional(window.Remaining), usagePlainOptional(window.Limit)
		if window.UsedPercent != nil {
			value := *window.UsedPercent
			if math.IsNaN(value) || math.IsInf(value, 0) {
				window.UsedPercent = nil
			} else {
				value = max(0, min(100, value))
				window.UsedPercent = &value
			}
		}
	}
	for i := range q.Balances {
		q.Balances[i].Unit = usagePlainLabel(q.Balances[i].Unit)
		q.Balances[i].Remaining = usagePlainLabel(q.Balances[i].Remaining)
	}
	return q
}

func usageQuotaError(status string) string {
	switch status {
	case "ready":
		return ""
	case "needs_auth", "no_credential":
		return tr("Authorization required")
	case "access_denied":
		return tr("Quota access denied")
	case "rate_limited":
		return tr("Quota refresh is rate limited")
	case "unsupported":
		return tr("Quota is not supported")
	case "no_data":
		return tr("No quota data available")
	default:
		return tr("Quota is unavailable")
	}
}

func usagePlainOptional(value *string) *string {
	if value == nil {
		return nil
	}
	text := usagePlainLabel(*value)
	if text == "" {
		return nil
	}
	return &text
}

func usagePlainLabel(text string) string {
	var clean strings.Builder
	count := 0
	for _, char := range strings.TrimSpace(text) {
		if unicode.IsControl(char) || unicode.Is(unicode.Cf, char) {
			continue
		}
		clean.WriteRune(char)
		count++
		if count == 200 {
			break
		}
	}
	return clean.String()
}
