package backend

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"path/filepath"
	"sync"
	"time"

	"fintwind-go/internal/backendstore"
)

const modelsCatalogURL = "https://models.dev/api.json"
const modelsCatalogLimit = 64 << 20
const modelsCatalogRefresh = 24 * time.Hour

type modelCatalogCache struct {
	mu         sync.Mutex
	data       json.RawMessage
	fetchedAt  time.Time
	retryAfter time.Time
	loaded     bool
	refreshing bool
}

func validModelsCatalog(raw json.RawMessage) bool {
	var entries map[string]json.RawMessage
	if len(raw) > modelsCatalogLimit || json.Unmarshal(raw, &entries) != nil {
		return false
	}
	for _, entry := range entries {
		var provider struct {
			Models map[string]json.RawMessage `json:"models"`
		}
		if json.Unmarshal(entry, &provider) == nil && len(provider.Models) > 0 {
			return true
		}
	}
	return false
}

func (s *Server) fetchModelsDevCatalog(ctx context.Context) (backendstore.Object, error) {
	cache := &s.modelCatalog
	cache.mu.Lock()
	defer cache.mu.Unlock()
	if !cache.loaded {
		cache.loaded = true
		stored, err := readObject(filepath.Join(s.Directory, "models-dev-catalog.json"))
		if err == nil && validModelsCatalog(stored["catalog"]) {
			cache.data = stored["catalog"]
			seconds, _ := stored.Int("fetched_at")
			cache.fetchedAt = time.Unix(int64(seconds), 0)
		}
	}
	if len(cache.data) > 0 {
		if time.Since(cache.fetchedAt) >= modelsCatalogRefresh && !cache.refreshing && time.Now().After(cache.retryAfter) {
			cache.refreshing = true
			go s.refreshModelsDevCatalog()
		}
		return backendstore.Reply("modelsDevCatalog", backendstore.Fields(map[string]any{"catalog": cache.data})), nil
	}
	raw, err := s.downloadModelsDevCatalog(ctx)
	if err != nil {
		return nil, err
	}
	s.saveModelsDevCatalog(raw)
	return backendstore.Reply("modelsDevCatalog", backendstore.Fields(map[string]any{"catalog": cache.data})), nil
}

func (s *Server) refreshModelsDevCatalog() {
	ctx, cancel := context.WithTimeout(s.lifetime, 40*time.Second)
	defer cancel()
	raw, err := s.downloadModelsDevCatalog(ctx)
	s.modelCatalog.mu.Lock()
	defer s.modelCatalog.mu.Unlock()
	s.modelCatalog.refreshing = false
	if err == nil {
		s.saveModelsDevCatalog(raw)
	} else {
		s.modelCatalog.retryAfter = time.Now().Add(10 * time.Minute)
	}
}

func (s *Server) saveModelsDevCatalog(raw json.RawMessage) {
	s.modelCatalog.data = raw
	s.modelCatalog.fetchedAt = time.Now()
	_ = writeObject(filepath.Join(s.Directory, "models-dev-catalog.json"), backendstore.Fields(map[string]any{"catalog": raw, "fetched_at": s.modelCatalog.fetchedAt.Unix()}))
}

func (s *Server) downloadModelsDevCatalog(ctx context.Context) (json.RawMessage, error) {
	s.fileMu.Lock()
	settings, err := readObject(s.settingsPath)
	s.fileMu.Unlock()
	if err != nil {
		return nil, err
	}
	proxy, err := managementProxy(settings)
	if err != nil {
		return nil, err
	}
	fetch := func(proxyURL string) (json.RawMessage, error) {
		transport := http.DefaultTransport.(*http.Transport).Clone()
		defer transport.CloseIdleConnections()
		if proxyURL != "" {
			parsed, parseErr := url.Parse(proxyURL)
			if parseErr != nil {
				return nil, parseErr
			}
			transport.Proxy = http.ProxyURL(parsed)
		}
		client := &http.Client{Transport: transport, Timeout: 15 * time.Second}
		request, requestErr := http.NewRequestWithContext(ctx, http.MethodGet, modelsCatalogURL, nil)
		if requestErr != nil {
			return nil, requestErr
		}
		response, requestErr := client.Do(request)
		if requestErr != nil {
			return nil, requestErr
		}
		defer response.Body.Close()
		if response.StatusCode != http.StatusOK {
			return nil, errors.New("models.dev catalog unavailable")
		}
		body, readErr := io.ReadAll(io.LimitReader(response.Body, modelsCatalogLimit+1))
		if readErr != nil {
			return nil, readErr
		}
		if !validModelsCatalog(body) {
			return nil, errors.New("invalid models.dev catalog")
		}
		return body, nil
	}
	raw, err := fetch(proxy["HTTPS_PROXY"])
	if err != nil && proxy["HTTPS_PROXY"] == "" && ctx.Err() == nil {
		return fetch("http://127.0.0.1:7897")
	}
	return raw, err
}
