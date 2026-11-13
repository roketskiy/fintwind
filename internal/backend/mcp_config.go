package backend

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"fintwind-go/internal/backendstore"
)

type mcpOAuth struct {
	Mode         string `json:"mode"`
	ClientID     string `json:"clientId"`
	ClientSecret string `json:"clientSecret"`
	Scope        string `json:"scope"`
}

type mcpServer struct {
	Name        string              `json:"name"`
	Kind        string              `json:"kind"`
	Command     []string            `json:"command"`
	URL         string              `json:"url"`
	Environment [][2]string         `json:"environment"`
	Headers     [][2]string         `json:"headers"`
	OAuth       mcpOAuth            `json:"oauth"`
	Enabled     bool                `json:"enabled"`
	Raw         backendstore.Object `json:"raw"`
}

func (s *Server) mcpConfigPath() (string, error) {
	var dir string
	if s.daily {
		dir = os.Getenv("OPENCODE_CONFIG_DIR")
		if dir == "" {
			home, err := os.UserHomeDir()
			if err != nil {
				return "", err
			}
			dir = filepath.Join(home, ".config", "opencode")
		}
	} else {
		dir = filepath.Join(filepath.Dir(s.Directory), "home", "opencode")
	}
	jsonc := filepath.Join(dir, "opencode.jsonc")
	if info, err := os.Stat(jsonc); err == nil && !info.IsDir() {
		return jsonc, nil
	}
	return filepath.Join(dir, "opencode.json"), nil
}

func mcpRevision(raw []byte) uint64 {
	hash := sha256.Sum256(raw)
	return binary.BigEndian.Uint64(hash[:8]) & (1<<63 - 1)
}

func mcpPairs(raw json.RawMessage) [][2]string {
	var values map[string]string
	_ = json.Unmarshal(raw, &values)
	keys := make([]string, 0, len(values))
	for key := range values {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	pairs := make([][2]string, 0, len(keys))
	for _, key := range keys {
		pairs = append(pairs, [2]string{key, values[key]})
	}
	return pairs
}

func mcpEntries(document backendstore.Object) (backendstore.Object, backendstore.Object) {
	mcp, _ := backendstore.Decode(document["mcp"])
	if mcp == nil {
		mcp = backendstore.Object{}
	}
	entries, _ := backendstore.Decode(mcp["servers"])
	if entries == nil {
		entries = mcp
	}
	return mcp, entries
}

func mcpRoster(document backendstore.Object) []mcpServer {
	_, entries := mcpEntries(document)
	names := make([]string, 0, len(entries))
	for name, raw := range entries {
		entry, err := backendstore.Decode(raw)
		if err == nil && (entry["type"] != nil || entry["command"] != nil || entry["url"] != nil) {
			names = append(names, name)
		}
	}
	sort.Strings(names)
	servers := make([]mcpServer, 0, len(names))
	for _, name := range names {
		entry, _ := backendstore.Decode(entries[name])
		server := mcpServer{Name: name, Kind: "local", Command: []string{}, URL: entry.String("url"), Environment: mcpPairs(entry["environment"]), Headers: mcpPairs(entry["headers"]), OAuth: mcpOAuth{Mode: "automatic"}, Enabled: !entry.Bool("disabled"), Raw: entry}
		if entry.String("type") == "remote" {
			server.Kind = "remote"
		}
		_ = json.Unmarshal(entry["command"], &server.Command)
		if server.Command == nil {
			server.Command = []string{}
		}
		if entry["enabled"] != nil && !entry.Bool("enabled") {
			server.Enabled = false
		}
		if string(entry["oauth"]) == "false" {
			server.OAuth.Mode = "disabled"
		} else if oauth, err := backendstore.Decode(entry["oauth"]); err == nil {
			server.OAuth.ClientID = oauth.String("client_id")
			if server.OAuth.ClientID == "" {
				server.OAuth.ClientID = oauth.String("clientId")
			}
			server.OAuth.ClientSecret = oauth.String("client_secret")
			if server.OAuth.ClientSecret == "" {
				server.OAuth.ClientSecret = oauth.String("clientSecret")
			}
			server.OAuth.Scope = oauth.String("scope")
			if server.OAuth.ClientID != "" || server.OAuth.ClientSecret != "" || server.OAuth.Scope != "" {
				server.OAuth.Mode = "custom"
			}
		}
		servers = append(servers, server)
	}
	return servers
}

func mcpTable(pairs [][2]string) map[string]string {
	table := map[string]string{}
	for _, pair := range pairs {
		if key := strings.TrimSpace(pair[0]); key != "" {
			table[key] = pair[1]
		}
	}
	return table
}

func (s *Server) mcpServers(command backendstore.Object) (backendstore.Object, error) {
	s.fileMu.Lock()
	defer s.fileMu.Unlock()
	path, err := s.mcpConfigPath()
	if err != nil {
		return nil, err
	}
	raw, err := os.ReadFile(path)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	doc := backendstore.Object{}
	if len(raw) > 0 {
		doc, err = backendstore.Decode(raw)
		if err != nil {
			return nil, err
		}
	}
	if command.String("type") == "saveMcpServers" {
		revision, err := command.Int("expectedRevision")
		if err != nil || uint64(revision) != mcpRevision(raw) {
			return nil, errors.New("MCP configuration changed; reload before saving")
		}
		var servers []mcpServer
		if json.Unmarshal(command["servers"], &servers) != nil || servers == nil {
			return nil, errors.New("invalid MCP server list")
		}
		mcp, old := mcpEntries(doc)
		for key, entry := range old {
			parsed, err := backendstore.Decode(entry)
			if err == nil && (parsed["type"] != nil || parsed["command"] != nil || parsed["url"] != nil) {
				delete(mcp, key)
			}
		}
		delete(mcp, "servers")
		entries := backendstore.Object{}
		for _, server := range servers {
			if strings.TrimSpace(server.Name) == "" || entries[server.Name] != nil || (server.Kind != "local" && server.Kind != "remote") {
				return nil, errors.New("invalid MCP server")
			}
			entry := server.Raw
			if entry == nil {
				entry = backendstore.Object{}
			}
			entry.Set("type", server.Kind)
			delete(entry, "enabled")
			delete(entry, "disabled")
			if !server.Enabled {
				entry.Set("disabled", true)
			}
			if server.Kind == "local" {
				entry.Set("command", server.Command)
				delete(entry, "url")
				delete(entry, "headers")
				delete(entry, "oauth")
				if values := mcpTable(server.Environment); len(values) > 0 {
					entry.Set("environment", values)
				} else {
					delete(entry, "environment")
				}
			} else {
				entry.Set("url", server.URL)
				delete(entry, "command")
				delete(entry, "environment")
				if values := mcpTable(server.Headers); len(values) > 0 {
					entry.Set("headers", values)
				} else {
					delete(entry, "headers")
				}
				switch server.OAuth.Mode {
				case "disabled":
					entry.Set("oauth", false)
				case "custom":
					oauth, _ := backendstore.Decode(entry["oauth"])
					if oauth == nil {
						oauth = backendstore.Object{}
					}
					delete(oauth, "clientId")
					delete(oauth, "clientSecret")
					for key, value := range map[string]string{"client_id": server.OAuth.ClientID, "client_secret": server.OAuth.ClientSecret, "scope": server.OAuth.Scope} {
						if value == "" {
							delete(oauth, key)
						} else {
							oauth.Set(key, value)
						}
					}
					entry.Set("oauth", oauth)
				default:
					delete(entry, "oauth")
				}
			}
			entries.Set(server.Name, entry)
		}
		if len(entries) > 0 {
			mcp.Set("servers", entries)
		}
		if len(mcp) > 0 {
			doc.Set("mcp", mcp)
		} else {
			delete(doc, "mcp")
		}
		if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
			return nil, err
		}
		if err := writeObject(path, doc); err != nil {
			return nil, err
		}
		raw, err = os.ReadFile(path)
		if err != nil {
			return nil, err
		}
	}
	return backendstore.Reply("mcpServers", backendstore.Fields(map[string]any{"servers": mcpRoster(doc), "revision": mcpRevision(raw)})), nil
}
