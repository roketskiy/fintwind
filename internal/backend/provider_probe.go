package backend

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendcommand"
	"fintwind-go/internal/backendstore"
)

func (s *Server) providerBinary(override string) (string, error) {
	binary := override
	if binary == "" {
		s.fileMu.Lock()
		settings, err := readObject(s.settingsPath)
		s.fileMu.Unlock()
		if err != nil {
			return "", err
		}
		binary = settings.String("opencode_binary_path")
	}
	if binary == "" {
		binary = "opencode"
	}
	found, err := exec.LookPath(binary)
	if err != nil {
		return "", err
	}
	absolute, err := filepath.Abs(found)
	if err != nil {
		return "", err
	}
	info, err := os.Stat(absolute)
	if err != nil || !info.Mode().IsRegular() {
		return "", os.ErrNotExist
	}
	return absolute, nil
}

func (s *Server) probeProvider(ctx context.Context, c backendstore.Object) (backendstore.Object, error) {
	binary, err := s.providerBinary(c.String("binaryOverride"))
	if err != nil {
		return backendstore.Reply("providerProbe", backendstore.Fields(map[string]any{"probe": map[string]any{"installed": false, "path": nil, "models": []any{}, "agent_presets": []any{}}, "version": nil})), nil
	}
	var version any
	if c.Bool("probeVersion") {
		output, err := backendcommand.Run(ctx, binary, []string{"--version"}, filepath.Dir(s.Directory), s.WorkspaceRoot, nil, 5*time.Second)
		if err == nil {
			for _, word := range strings.Fields(string(output)) {
				token := strings.Trim(strings.TrimPrefix(word, "v"), "(),;")
				if len(token) > 0 && token[0] >= '0' && token[0] <= '9' && strings.Count(token, ".") >= 2 {
					version = token
					break
				}
			}
		}
	}
	if c.Bool("discoverModels") {
		copy := backendstore.Object{}
		for key, value := range c {
			copy[key] = value
		}
		copy.Set("binaryOverride", binary)
		raw, _ := json.Marshal(copy)
		response, _, err := s.services.Handle(ctx, raw)
		if err != nil {
			return nil, err
		}
		result, err := backendstore.Decode(response)
		if err != nil {
			return nil, err
		}
		result.Set("version", version)
		return result, nil
	}
	return backendstore.Reply("providerProbe", backendstore.Fields(map[string]any{"probe": map[string]any{"installed": true, "path": binary, "models": []any{}, "agent_presets": []any{}}, "version": version})), nil
}
