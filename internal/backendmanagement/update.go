// Package backendmanagement contains explicit host CLI management operations.
package backendmanagement

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"time"
)

const LatestURL = "https://opencode.ai/update/api/latest/cli/npm"

type Doer interface {
	Do(*http.Request) (*http.Response, error)
}

func Latest(ctx context.Context, client Doer, endpoint string) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, "GET", endpoint, nil)
	if err != nil {
		return "", errors.New("invalid update metadata endpoint")
	}
	request.Header.Set("Accept", "application/json")
	response, err := client.Do(request)
	if err != nil {
		return "", errors.New("OpenCode update metadata unavailable")
	}
	defer response.Body.Close()
	if response.StatusCode != 200 {
		return "", errors.New("OpenCode update metadata request refused")
	}
	raw, err := io.ReadAll(io.LimitReader(response.Body, (1<<20)+1))
	if err != nil || len(raw) > 1<<20 {
		return "", errors.New("OpenCode update metadata exceeds budget")
	}
	var metadata struct {
		Version string `json:"version"`
	}
	if json.Unmarshal(raw, &metadata) != nil || !SupportedVersion(metadata.Version) {
		return "", errors.New("OpenCode update metadata has an invalid or unsupported version")
	}
	return metadata.Version, nil
}
