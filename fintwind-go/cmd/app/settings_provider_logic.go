package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode"
)

var providerInputModalities = []string{"text", "image", "audio", "video", "pdf"}
var providerOutputModalities = []string{"text", "image", "audio", "video"}

const providerConnectivityTimeout = 20 * time.Second
const providerListTimeout = 120 * time.Second
const providerFirstTokenTimeout = 30 * time.Second

type providerAPIModel struct {
	id            string
	name          *string
	contextWindow *uint64
	outputLimit   *uint64
}

type apiListError struct {
	kind    string
	status  int
	message string
}

func (e apiListError) text() string {
	switch e.kind {
	case "invalid_url":
		return tr("The Base URL is not an http(s) address")
	case "auth":
		return trf("the endpoint rejected the API key (HTTP %d)", e.status)
	case "missing":
		return tr("the model-list path was not found (HTTP 404), check the Base URL")
	case "status":
		return trf("HTTP %d", e.status)
	case "unreachable":
		return trf("no response from the endpoint: %s", e.message)
	case "nolist":
		return trf("the response carries no model list: %s", e.message)
	default:
		return e.message
	}
}

type firstTokenError struct {
	kind    string
	status  int
	message *string
}

func (e firstTokenError) text() string {
	switch e.kind {
	case "invalid_url":
		return tr("The Base URL is not an http(s) address")
	case "status":
		var wording string
		switch e.status {
		case 401, 403:
			wording = trf("the API key was rejected (HTTP %d)", e.status)
		case 404:
			wording = tr("the model was not found (HTTP 404)")
		default:
			wording = trf("HTTP %d", e.status)
		}
		if e.message != nil && *e.message != "" {
			return wording + " · " + *e.message
		}
		return wording
	case "unreachable":
		msg := ""
		if e.message != nil {
			msg = *e.message
		}
		return trf("no response from the endpoint: %s", msg)
	case "nostream":
		detail := ""
		if e.message != nil && *e.message != "" {
			detail = " · " + *e.message
		}
		return tr("the endpoint returned no streamed tokens") + detail
	case "timeout":
		return trf("no first token within %d s", 30)
	default:
		if e.message != nil {
			return *e.message
		}
		return tr("Failed")
	}
}

func baseURLValid(raw string) bool {
	return strings.HasPrefix(raw, "http://") || strings.HasPrefix(raw, "https://")
}

func providerSlug(name string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(strings.TrimSpace(name)) {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9', r == '_' || r == '-':
			b.WriteRune(r)
		case r == ' ' || r == '.' || r == '-':
			b.WriteByte('-')
		case unicode.IsLetter(r) || unicode.IsDigit(r):
			b.WriteRune(r)
		}
	}
	slug := strings.Trim(b.String(), "-")
	if len(slug) > 40 {
		slug = slug[:40]
	}
	slug = strings.Trim(slug, "-")
	if strings.Trim(slug, "-") == "" {
		return "p"
	}
	return slug
}

func uniqueProviderSlug(name string, taken []string) string {
	base := providerSlug(name)
	set := map[string]bool{}
	for _, id := range taken {
		set[id] = true
	}
	if !set[base] {
		return base
	}
	for i := 2; ; i++ {
		candidate := fmt.Sprintf("%s-%d", base, i)
		if !set[candidate] {
			return candidate
		}
	}
}

func parseContextWindow(text string) *uint64 {
	trimmed := strings.TrimSpace(text)
	if trimmed == "" {
		return nil
	}
	multiplier := uint64(1)
	number := trimmed
	if last := trimmed[len(trimmed)-1]; last >= '0' && last <= '9' {
	} else if last == 'k' || last == 'K' {
		multiplier, number = 1000, trimmed[:len(trimmed)-1]
	} else if last == 'm' || last == 'M' {
		multiplier, number = 1000000, trimmed[:len(trimmed)-1]
	} else {
		return nil
	}
	number = strings.ReplaceAll(number, ",", "")
	value, err := strconv.ParseUint(strings.TrimSpace(number), 10, 64)
	if err != nil || value == 0 {
		return nil
	}
	out := value * multiplier
	if out == 0 {
		return nil
	}
	return &out
}

func formatContextWindow(tokens uint64) string {
	if tokens >= 1000000 && tokens%1000000 == 0 {
		return fmt.Sprintf("%dM", tokens/1000000)
	}
	if tokens >= 1000 && tokens%1000 == 0 {
		return fmt.Sprintf("%dK", tokens/1000)
	}
	return strconv.FormatUint(tokens, 10)
}

func formatProbeLatency(d time.Duration) string {
	millis := d.Milliseconds()
	if millis < 1000 {
		return fmt.Sprintf("%d ms", millis)
	}
	return fmt.Sprintf("%.2f s", d.Seconds())
}

func versionedRoot(base string) string {
	if strings.HasSuffix(base, "/v1") {
		return base
	}
	return base + "/v1"
}

func betaRoot(base string) string {
	if strings.HasSuffix(base, "/v1beta") || strings.HasSuffix(base, "/v1") {
		return base
	}
	return base + "/v1beta"
}

func geminiModelPath(modelID string) string {
	if strings.HasPrefix(modelID, "models/") {
		return modelID
	}
	return "models/" + modelID
}

func providerAuthHeaders(format, apiKey string) []string {
	headers := []string{"User-Agent: fintwind"}
	key := strings.TrimSpace(apiKey)
	switch format {
	case "openai", "openai-responses":
		if key != "" {
			headers = append(headers, "Authorization: Bearer "+key)
		}
	case "anthropic":
		if key != "" {
			headers = append(headers, "x-api-key: "+key)
		}
		headers = append(headers, "anthropic-version: 2023-06-01")
	case "google":
		if key != "" {
			headers = append(headers, "x-goog-api-key: "+key)
		}
	}
	return headers
}

func providerListRequest(format, baseURL, apiKey string) (string, []string) {
	base := strings.TrimSuffix(strings.TrimSpace(baseURL), "/")
	var endpoint string
	switch format {
	case "anthropic":
		endpoint = versionedRoot(base) + "/models"
	case "google":
		endpoint = betaRoot(base) + "/models"
	default:
		endpoint = base + "/models"
	}
	headers := append(providerAuthHeaders(format, apiKey), "Accept: application/json")
	return endpoint, headers
}

func providerHTTPGet(endpoint string, headers []string, timeout time.Duration) (int, string, error) {
	req, err := http.NewRequest("GET", endpoint, nil)
	if err != nil {
		return 0, "", err
	}
	for _, header := range headers {
		if name, value, ok := strings.Cut(header, ": "); ok {
			req.Header.Set(strings.TrimSpace(name), strings.TrimSpace(value))
		}
	}
	client := &http.Client{Timeout: timeout}
	resp, err := client.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(io.LimitReader(resp.Body, 8*1024*1024))
	if err != nil {
		return 0, "", err
	}
	return resp.StatusCode, string(body), nil
}

func parseProviderModelList(body string, format string) ([]providerAPIModel, error) {
	var document map[string]any
	decoder := json.NewDecoder(strings.NewReader(body))
	decoder.UseNumber()
	if err := decoder.Decode(&document); err != nil {
		return nil, err
	}
	entries, _ := document["data"].([]any)
	if entries == nil {
		entries, _ = document["models"].([]any)
	}
	var out []providerAPIModel
	for _, entry := range entries {
		item, ok := entry.(map[string]any)
		if !ok {
			continue
		}
		id, _ := item["id"].(string)
		if id == "" {
			id, _ = item["name"].(string)
		}
		if id == "" {
			continue
		}
		model := providerAPIModel{id: id}
		if name, _ := item["name"].(string); name != "" && name != id {
			n := name
			model.name = &n
		} else if display, _ := item["display_name"].(string); display != "" && display != id {
			n := display
			model.name = &n
		}
		model.contextWindow = providerModelLimit(item, "context", "context_window", "contextWindow")
		model.outputLimit = providerModelLimit(item, "output", "output_limit", "outputLimit", "max_output_tokens")
		out = append(out, model)
	}
	if len(out) == 0 {
		return nil, fmt.Errorf("the response carries no model list entries")
	}
	return out, nil
}

func providerModelLimit(item map[string]any, keys ...string) *uint64 {
	for _, key := range keys {
		switch value := item[key].(type) {
		case float64:
			if value > 0 {
				n := uint64(value)
				return &n
			}
		case json.Number:
			if n, err := value.Int64(); err == nil && n > 0 {
				u := uint64(n)
				return &u
			}
		}
	}
	if limit, ok := item["limit"].(map[string]any); ok {
		for _, key := range []string{"context", "output"} {
			isWanted := false
			for _, want := range keys {
				if want == key || want == "context_window" && key == "context" || want == "output_limit" && key == "output" {
					isWanted = true
				}
			}
			if !isWanted {
				continue
			}
			if value, ok := limit[key].(float64); ok && value > 0 {
				n := uint64(value)
				return &n
			}
			if value, ok := limit[key].(json.Number); ok {
				if n, err := value.Int64(); err == nil && n > 0 {
					u := uint64(n)
					return &u
				}
			}
		}
	}
	return nil
}

func fetchProviderAPIModels(format, baseURL, apiKey string, timeout time.Duration) ([]providerAPIModel, *apiListError) {
	if !baseURLValid(strings.TrimSpace(baseURL)) {
		return nil, &apiListError{kind: "invalid_url"}
	}
	endpoint, headers := providerListRequest(format, baseURL, apiKey)
	status, body, err := providerHTTPGet(endpoint, headers, timeout)
	if err != nil {
		return nil, &apiListError{kind: "unreachable", message: err.Error()}
	}
	switch status {
	case 200:
		models, err := parseProviderModelList(body, format)
		if err != nil {
			return nil, &apiListError{kind: "nolist", message: err.Error()}
		}
		return models, nil
	case 401, 403:
		return nil, &apiListError{kind: "auth", status: status}
	case 404:
		return nil, &apiListError{kind: "missing", status: status}
	default:
		return nil, &apiListError{kind: "status", status: status}
	}
}

type connectivityOutcome struct {
	reachable bool
	models    int
	latency   time.Duration
	err       *apiListError
}

func probeProviderConnectivity(format, baseURL, apiKey string) connectivityOutcome {
	started := time.Now()
	models, err := fetchProviderAPIModels(format, baseURL, apiKey, providerConnectivityTimeout)
	if err != nil {
		return connectivityOutcome{err: err}
	}
	return connectivityOutcome{reachable: true, models: len(models), latency: time.Since(started)}
}

func providerFirstTokenRequest(format, baseURL, modelID, apiKey string) (string, []string, string) {
	base := strings.TrimSuffix(strings.TrimSpace(baseURL), "/")
	var endpoint string
	switch format {
	case "openai-responses":
		endpoint = base + "/responses"
	case "anthropic":
		endpoint = versionedRoot(base) + "/messages"
	case "google":
		endpoint = betaRoot(base) + "/" + geminiModelPath(modelID) + ":streamGenerateContent?alt=sse"
	default:
		endpoint = base + "/chat/completions"
	}
	headers := append(providerAuthHeaders(format, apiKey), "Content-Type: application/json", "Accept: text/event-stream")
	escaped, _ := json.Marshal(modelID)
	var body string
	switch format {
	case "openai-responses":
		body = fmt.Sprintf(`{"model":%s,"input":"Hi","stream":true}`, string(escaped))
	case "anthropic":
		body = fmt.Sprintf(`{"model":%s,"max_tokens":1,"stream":true,"messages":[{"role":"user","content":"Hi"}]}`, string(escaped))
	case "google":
		body = `{"contents":[{"role":"user","parts":[{"text":"Hi"}]}]}`
	default:
		body = fmt.Sprintf(`{"model":%s,"stream":true,"messages":[{"role":"user","content":"Hi"}]}`, string(escaped))
	}
	return endpoint, headers, body
}

func probeProviderFirstToken(format, baseURL, modelID, apiKey string) (time.Duration, *firstTokenError) {
	if !baseURLValid(strings.TrimSpace(baseURL)) {
		return 0, &firstTokenError{kind: "invalid_url"}
	}
	endpoint, headers, body := providerFirstTokenRequest(format, baseURL, modelID, apiKey)
	if _, err := url.Parse(endpoint); err != nil {
		return 0, &firstTokenError{kind: "invalid_url"}
	}
	req, err := http.NewRequest("POST", endpoint, bytes.NewReader([]byte(body)))
	if err != nil {
		return 0, &firstTokenError{kind: "unreachable", message: strPtr(err.Error())}
	}
	for _, header := range headers {
		if name, value, ok := strings.Cut(header, ": "); ok {
			req.Header.Set(strings.TrimSpace(name), strings.TrimSpace(value))
		}
	}
	client := &http.Client{Timeout: providerFirstTokenTimeout}
	started := time.Now()
	resp, err := client.Do(req)
	if err != nil {
		msg := err.Error()
		if strings.Contains(msg, "deadline") || strings.Contains(msg, "timeout") {
			return 0, &firstTokenError{kind: "timeout"}
		}
		return 0, &firstTokenError{kind: "unreachable", message: &msg}
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 {
		raw, _ := io.ReadAll(io.LimitReader(resp.Body, 64*1024))
		msg := providerErrorMessage(string(raw))
		return 0, &firstTokenError{kind: "status", status: resp.StatusCode, message: msg}
	}
	deadline := time.Now().Add(providerFirstTokenTimeout)
	buffer := make([]byte, 4096)
	for time.Now().Before(deadline) {
		n, err := resp.Body.Read(buffer)
		if n > 0 && bytes.Contains(buffer[:n], []byte("data:")) {
			return time.Since(started), nil
		}
		if err != nil {
			break
		}
	}
	msg := strPtr("")
	return 0, &firstTokenError{kind: "nostream", message: msg}
}

func providerErrorMessage(body string) *string {
	var value map[string]any
	if json.Unmarshal([]byte(body), &value) != nil {
		trimmed := strings.TrimSpace(body)
		if trimmed == "" {
			return nil
		}
		if len(trimmed) > 300 {
			trimmed = trimmed[:300]
		}
		return &trimmed
	}
	var message string
	if nested, ok := value["error"].(map[string]any); ok {
		message, _ = nested["message"].(string)
	}
	if message == "" {
		message, _ = value["message"].(string)
	}
	if message == "" {
		if text, _ := value["error"].(string); text != "" {
			message = text
		}
	}
	message = strings.TrimSpace(message)
	if message == "" {
		return nil
	}
	if len(message) > 300 {
		message = message[:300]
	}
	return &message
}

func strPtr(s string) *string { return &s }

func toggleModalityList(selected []string, modality string, canonical []string) []string {
	for i, entry := range selected {
		if entry == modality {
			return append(append([]string{}, selected[:i]...), selected[i+1:]...)
		}
	}
	out := append(append([]string{}, selected...), modality)
	order := map[string]int{}
	for i, name := range canonical {
		order[name] = i
	}
	sort.SliceStable(out, func(i, j int) bool {
		oi, oki := order[out[i]]
		oj, okj := order[out[j]]
		if oki && okj {
			return oi < oj
		}
		if oki {
			return true
		}
		if okj {
			return false
		}
		return out[i] < out[j]
	})
	return out
}

func customModelDisplayName(name *string, id string) *string {
	if name == nil {
		return nil
	}
	trimmed := strings.TrimSpace(*name)
	if trimmed == "" || trimmed == id {
		return nil
	}
	return name
}
