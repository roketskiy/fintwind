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

type API interface {
	RoundTrip(context.Context, string, string, any) (json.RawMessage, error)
}
type Acquire func(context.Context, backendstore.Object) (API, error)
type Service struct {
	root        string
	acquire     Acquire
	generations generations
}

func New(root string, acquire Acquire) *Service { return &Service{root: root, acquire: acquire} }

func (s *Service) Handle(ctx context.Context, raw json.RawMessage) (json.RawMessage, bool, error) {
	c, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	if kind := c.String("type"); kind == "generateSessionText" || kind == "cancelSessionGeneration" {
		var result backendstore.Object
		if kind == "cancelSessionGeneration" {
			err = s.generations.cancel(c.String("generationId"))
			result = backendstore.Reply("ack", nil)
		} else {
			result, err = s.generate(ctx, c)
		}
		if err != nil {
			return nil, true, err
		}
		response, err := json.Marshal(result)
		return response, true, err
	}
	switch c.String("type") {
	case "probeProvider", "fetchImageModels", "probeBuiltinProvider", "listProviderSessions", "renameProviderSession", "deleteProviderSession", "fetchIntegrations", "authorizeProvider", "startProviderOAuth", "pollProviderOAuth", "completeProviderOAuth", "cancelProviderOAuth", "listMcpServerStatuses", "reloadOpenCodeServer", "activateProviderKeyAccount", "storeCustomProviderKey", "resolveCustomProviderKey", "logoutProvider", "forkProviderSession", "fetchNativeTranscript", "fetchNativeTurnStats", "fetchNativeUsage":
	default:
		return nil, false, nil
	}
	options := c
	if c.String("type") == "forkProviderSession" {
		request, e := backendstore.Decode(c["request"])
		if e != nil {
			return nil, true, e
		}
		options = backendstore.Fields(map[string]any{"binary": request.String("binary"), "directory": request.String("cwd")})
	}
	api, err := s.acquire(ctx, options)
	if err != nil {
		return nil, true, err
	}
	result, err := s.dispatch(ctx, api, c)
	if err != nil {
		return nil, true, err
	}
	response, err := json.Marshal(result)
	return response, true, err
}

func data(raw json.RawMessage) (json.RawMessage, error) {
	if len(raw) > 0 && raw[0] == '[' {
		return raw, nil
	}
	var response struct {
		Data json.RawMessage `json:"data"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return nil, err
	}
	if len(response.Data) == 0 {
		return raw, nil
	}
	return response.Data, nil
}

func (s *Service) dispatch(ctx context.Context, api API, c backendstore.Object) (backendstore.Object, error) {
	kind := c.String("type")
	provider := url.PathEscape(c.String("providerId"))
	session := url.PathEscape(c.String("sessionId"))
	attempt := url.PathEscape(c.String("attemptId"))
	call := func(method, path string, body any) (json.RawMessage, error) {
		return api.RoundTrip(ctx, method, path, body)
	}
	switch kind {
	case "fetchNativeUsage":
		reader, ok := api.(interface {
			NativeUsage(context.Context, string) (*model.ContextUsage, error)
		})
		if !ok {
			return nil, errors.New("native usage adapter unavailable")
		}
		usage, err := reader.NativeUsage(ctx, c.String("sessionId"))
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("nativeUsage", backendstore.Fields(map[string]any{"usage": usage})), nil
	case "fetchNativeTurnStats":
		return fetchTurnStats(ctx, api, c)
	case "forkProviderSession":
		request, err := backendstore.Decode(c["request"])
		if err != nil {
			return nil, err
		}
		return nativeFork(ctx, api, request)
	case "fetchNativeTranscript":
		return fetchNativeTranscript(ctx, api, c.String("sessionId"))
	case "probeProvider", "fetchImageModels", "probeBuiltinProvider":
		path := "/api/model"
		if kind == "fetchImageModels" {
			path += "?types=image"
		}
		raw, err := call("GET", path, nil)
		if err != nil {
			return nil, err
		}
		body, err := data(raw)
		if err != nil {
			return nil, err
		}
		var entries []backendstore.Object
		if err := json.Unmarshal(body, &entries); err != nil {
			return nil, errors.New("invalid model catalog")
		}
		models := []backendstore.Object{}
		imageModels := []backendstore.Object{}
		count := 0
		for _, entry := range entries {
			id := entry.String("id")
			owner := entry.String("providerID")
			if owner == "" {
				owner = entry.String("providerId")
			}
			if id == "" || owner == "" {
				continue
			}
			if kind == "fetchImageModels" {
				if entry.String("type") == "image" {
					label := entry.String("name")
					if label == "" {
						label = id
					}
					imageModels = append(imageModels, backendstore.Fields(map[string]any{"provider": owner, "id": owner + "/" + id, "label": label}))
				}
				continue
			}
			if owner == c.String("providerId") {
				count++
			}
			label := entry.String("name")
			if label == "" {
				label = id
			}
			models = append(models, backendstore.Fields(map[string]any{"id": owner + "/" + id, "name": label, "sub_provider": owner, "reasoning_efforts": []any{}, "service_tiers": []any{}, "context_windows": []any{}}))
		}
		if kind == "probeBuiltinProvider" {
			if count == 0 {
				return nil, errors.New("provider has no available models")
			}
			return backendstore.Reply("builtinProviderProbed", backendstore.Fields(map[string]any{"models": count})), nil
		}
		binary := c.String("binaryOverride")
		if binary == "" {
			binary = c.String("binary")
		}
		probe := backendstore.Fields(map[string]any{"installed": true, "path": binary, "models": models, "agent_presets": []any{}})
		if kind == "fetchImageModels" {
			return backendstore.Reply("json", backendstore.Fields(map[string]any{"value": imageModels})), nil
		}
		return backendstore.Reply("providerProbe", backendstore.Fields(map[string]any{"probe": probe, "version": nil})), nil
	case "listProviderSessions":
		rows := []backendstore.Object{}
		seen := map[string]bool{}
		cursor := ""
		for page := 0; page < 10000; page++ {
			path := "/api/session?directory=" + url.QueryEscape(c.String("directory")) + "&limit=200&parentID=null"
			if cursor != "" {
				path += "&cursor=" + url.QueryEscape(cursor)
			}
			raw, err := call("GET", path, nil)
			if err != nil {
				return nil, err
			}
			var response struct {
				Data   []backendstore.Object `json:"data"`
				Cursor struct {
					Next string `json:"next"`
				} `json:"cursor"`
			}
			if err := json.Unmarshal(raw, &response); err != nil {
				return nil, err
			}
			for _, row := range response.Data {
				var times struct {
					Created uint64 `json:"created"`
					Updated uint64 `json:"updated"`
				}
				_ = json.Unmarshal(row["time"], &times)
				location, _ := backendstore.Decode(row["location"])
				rows = append(rows, backendstore.Fields(map[string]any{"sessionId": row.String("id"), "title": row.Nullable("title"), "createdAt": times.Created / 1000, "updatedAt": times.Updated / 1000, "directory": location.Nullable("directory")}))
			}
			if response.Cursor.Next == "" {
				break
			}
			if seen[response.Cursor.Next] || len(response.Data) == 0 {
				return nil, errors.New("session list cursor stalled")
			}
			seen[response.Cursor.Next] = true
			cursor = response.Cursor.Next
			if page == 9999 {
				return nil, errors.New("session list limit reached")
			}
		}
		return backendstore.Reply("providerSessions", backendstore.Fields(map[string]any{"sessions": rows})), nil
	case "renameProviderSession":
		_, err := call("PATCH", "/api/session/"+session, map[string]string{"title": c.String("title")})
		return backendstore.Reply("ack", nil), err
	case "deleteProviderSession":
		_, err := call("DELETE", "/api/session/"+session, nil)
		return backendstore.Reply("ack", nil), err
	case "authorizeProvider":
		_, err := call("POST", "/api/integration/"+provider+"/connect/key", map[string]string{"key": c.String("key")})
		return backendstore.Reply("ack", nil), err
	case "startProviderOAuth", "pollProviderOAuth", "completeProviderOAuth", "cancelProviderOAuth":
		path := "/api/integration/" + provider + "/connect/oauth"
		method := "POST"
		var body any
		response := "providerOAuthPolled"
		field := "poll"
		switch kind {
		case "startProviderOAuth":
			response = "providerOAuthStarted"
			field = "attempt"
			body = map[string]any{"methodID": c.String("methodId"), "answer": c["answers"]}
		case "pollProviderOAuth":
			method = "GET"
			path += "/" + attempt
		case "completeProviderOAuth":
			path += "/" + attempt + "/complete"
			body = map[string]any{"code": c.Nullable("code")}
		case "cancelProviderOAuth":
			method = "DELETE"
			path += "/" + attempt
		}
		raw, err := call(method, path, body)
		if err != nil {
			return nil, err
		}
		if kind == "cancelProviderOAuth" || kind == "completeProviderOAuth" {
			return backendstore.Reply("ack", nil), nil
		}
		value, err := data(raw)
		if err != nil {
			return nil, err
		}
		object, err := backendstore.Decode(value)
		if err != nil {
			return nil, err
		}
		if kind == "startProviderOAuth" {
			id := object.String("attemptID")
			if id == "" {
				id = object.String("attemptId")
			}
			if id == "" || object.String("url") == "" {
				return nil, errors.New("invalid OAuth attempt")
			}
			mode := "auto"
			if object.String("mode") == "code" {
				mode = "code"
			}
			object = backendstore.Fields(map[string]any{"attemptId": id, "url": object.String("url"), "instructions": object.String("instructions"), "mode": mode})
		} else {
			switch object.String("status") {
			case "pending", "complete", "expired", "failed":
			default:
				return nil, errors.New("invalid OAuth status")
			}
		}
		return backendstore.Reply(response, backendstore.Fields(map[string]any{field: object})), nil
	case "reloadOpenCodeServer":
		_, err := call("POST", "/api/location/reload", nil)
		return backendstore.Reply("ack", nil), err
	case "listMcpServerStatuses":
		raw, err := call("GET", "/api/mcp?directory="+url.QueryEscape(c.String("directory")), nil)
		if err != nil {
			return nil, err
		}
		value, err := data(raw)
		if err != nil {
			return nil, err
		}
		var rows []backendstore.Object
		if err := json.Unmarshal(value, &rows); err != nil {
			return nil, err
		}
		return backendstore.Reply("mcpServerStatuses", backendstore.Fields(map[string]any{"statuses": rows})), nil
	case "activateProviderKeyAccount":
		id := c.String("credentialId")
		if id == "" {
			return nil, errors.New("credential ID required")
		}
		rows, err := credentialRows(ctx, api)
		if err != nil {
			return nil, err
		}
		valid := false
		for _, row := range rows {
			secret, _ := backendstore.Decode(row["value"])
			if row.String("id") == id && row.String("integrationID") == c.String("providerId") && secret.String("type") == "key" {
				valid = true
			}
		}
		if !valid {
			return nil, errors.New("provider account not found")
		}
		_, err = call("POST", "/api/credential/"+url.PathEscape(id)+"/activate", nil)
		return backendstore.Reply("ack", nil), err
	case "storeCustomProviderKey":
		err := storeCredential(ctx, api, c.String("providerId"), c.String("key"))
		return backendstore.Reply("ack", nil), err
	case "resolveCustomProviderKey", "logoutProvider":
		raw, err := call("GET", "/api/credential", nil)
		if err != nil {
			return nil, err
		}
		value, err := data(raw)
		if err != nil {
			return nil, err
		}
		var rows []backendstore.Object
		if err := json.Unmarshal(value, &rows); err != nil {
			return nil, err
		}
		var key any
		for _, row := range rows {
			if row.String("integrationID") != c.String("providerId") {
				continue
			}
			if kind == "logoutProvider" {
				if strings.HasPrefix(row.String("id"), "fintwind-custom:"+c.String("providerId")+":") {
					if _, err := call("DELETE", "/api/credential/"+url.PathEscape(row.String("id")), nil); err != nil {
						return nil, err
					}
				}
			} else if row.Bool("active") {
				secret, _ := backendstore.Decode(row["value"])
				key = secret.Nullable("key")
			}
		}
		if kind == "logoutProvider" {
			return backendstore.Reply("ack", nil), nil
		}
		return backendstore.Reply("json", backendstore.Fields(map[string]any{"value": key})), nil
	case "fetchIntegrations":
		raw, err := call("GET", "/api/integration", nil)
		if err != nil {
			return nil, err
		}
		value, err := data(raw)
		if err != nil {
			return nil, err
		}
		rows, err := integrationSummaries(value)
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("integrations", backendstore.Fields(map[string]any{"integrations": rows})), nil
	}
	return nil, errors.New("service command missing")
}

func integrationSummaries(raw json.RawMessage) ([]backendstore.Object, error) {
	var rows []backendstore.Object
	if err := json.Unmarshal(raw, &rows); err != nil {
		return nil, err
	}
	result := make([]backendstore.Object, 0, len(rows))
	for _, row := range rows {
		var methods, connections []backendstore.Object
		_ = json.Unmarshal(row["methods"], &methods)
		_ = json.Unmarshal(row["connections"], &connections)
		key := false
		oauth := []backendstore.Object{}
		for _, method := range methods {
			if method.String("type") == "key" {
				key = true
			}
			if method.String("type") == "oauth" {
				oauth = append(oauth, method)
			}
		}
		env := []string{}
		accounts := []backendstore.Object{}
		needsAuth := len(connections) > 0
		var authURL any
		for index, connection := range connections {
			status, _ := backendstore.Decode(connection["status"])
			if status.String("status") != "needs_auth" {
				needsAuth = false
			} else if status.String("url") != "" {
				authURL = status.String("url")
			}
			if connection.String("type") == "env" {
				env = append(env, connection.String("name"))
			}
			if connection.String("type") == "credential" && connection.String("method") == "key" {
				accounts = append(accounts, backendstore.Fields(map[string]any{"id": connection.String("id"), "label": connection.String("label"), "active": index == 0}))
			}
		}
		result = append(result, backendstore.Fields(map[string]any{"id": row.String("id"), "name": row.String("name"), "supportsKey": key, "connected": len(connections) > 0, "needsAuth": needsAuth, "authUrl": authURL, "envNames": env, "keyAccounts": accounts, "oauthMethods": oauth}))
	}
	return result, nil
}
