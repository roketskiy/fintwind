package backendengine

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"fintwind-go/internal/backendprivate"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

type Process struct {
	Client *Client
	owned  *ownedprocess.Owned
}

func (p *Process) Close() error { p.Client.Close(); return p.owned.Close() }

func Start(ctx context.Context, binary, root, workspace, settingsPath string, daily bool) (*Process, error) {
	if !filepath.IsAbs(binary) || !filepath.IsAbs(root) || !filepath.IsAbs(workspace) {
		return nil, errors.New("absolute engine paths required")
	}
	canonical, err := filepath.EvalSymlinks(workspace)
	if err != nil {
		return nil, errors.New("workspace not accessible")
	}
	base, err := filepath.EvalSymlinks(root)
	if err != nil {
		return nil, err
	}
	logRoot := root
	if daily {
		logRoot = filepath.Join(root, "opencode-serve")
		if err := os.MkdirAll(logRoot, 0o700); err != nil {
			return nil, err
		}
	} else {
		rel, err := filepath.Rel(base, canonical)
		if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
			return nil, errors.New("engine workspace outside isolated profile")
		}
		if err := backendprivate.Directory(base); err != nil {
			return nil, errors.New("cannot protect isolated engine directory")
		}
		for _, sub := range []string{"data", "home/opencode", "tmp"} {
			if err := os.MkdirAll(filepath.Join(root, sub), 0o700); err != nil {
				return nil, err
			}
		}
	}
	logs, err := os.MkdirTemp(logRoot, "engine-")
	if err != nil {
		return nil, err
	}
	password := uuid.NewString()
	extra := map[string]string{
		"OPENCODE_SERVER_USERNAME": "opencode", "OPENCODE_SERVER_PASSWORD": password,
		"OPENCODE_CONFIG_DIR":             filepath.Join(root, "home", "opencode"),
		"OPENCODE_DB":                     filepath.Join(root, "data", "opencode.db"),
		"OPENCODE_TEST_HOME":              filepath.Join(root, "home"),
		"OPENCODE_DISABLE_PROJECT_CONFIG": "true",
	}
	// Real-provider E2E is opt-in at process launch, never through an RPC.
	// The default launcher still drops every provider credential. Only this
	// explicitly authorized provider may cross into this owned engine Job.
	if !daily && os.Getenv("FINTWIND_AGENT_REAL_PROVIDER") == "deepseek" {
		if os.Getenv("FINTWIND_MYGO_PROFILE") != "agent" {
			return nil, errors.New("real provider requires isolated agent profile")
		}
		key := os.Getenv("DEEPSEEK_API_KEY")
		if strings.TrimSpace(key) == "" {
			return nil, errors.New("authorized DeepSeek credential missing")
		}
		extra["DEEPSEEK_API_KEY"] = key
		for _, name := range []string{"HTTP_PROXY", "HTTPS_PROXY"} {
			if value := os.Getenv(name); value != "" {
				extra[name] = value
			}
		}
	}
	environment := ownedprocess.BuildEnv(root, extra)
	if daily {
		values := map[string]string{"OPENCODE_SERVER_USERNAME": "opencode", "OPENCODE_SERVER_PASSWORD": password}
		raw, readErr := os.ReadFile(settingsPath)
		if readErr != nil && !errors.Is(readErr, os.ErrNotExist) {
			return nil, readErr
		}
		if len(raw) > 0 {
			var settings struct {
				Proxy string `json:"network_proxy_url"`
			}
			if err := json.Unmarshal(raw, &settings); err != nil {
				return nil, errors.New("invalid daemon settings")
			}
			if settings.Proxy != "" {
				proxy, err := url.Parse(settings.Proxy)
				if err != nil || proxy.Host == "" || (proxy.Scheme != "http" && proxy.Scheme != "https" && proxy.Scheme != "socks5" && proxy.Scheme != "socks5h") {
					return nil, errors.New("invalid configured network proxy")
				}
				values["HTTP_PROXY"], values["HTTPS_PROXY"], values["NO_PROXY"] = settings.Proxy, settings.Proxy, "localhost,127.0.0.1,::1"
			}
		}
		environment = ownedprocess.InheritEnv(values)
	}
	child, err := ownedprocess.StartWithEnvironment(binary, []string{"serve", "--hostname", "127.0.0.1", "--port", "0"}, logs, logRoot, environment)
	if err != nil {
		return nil, err
	}
	var client *Client
	announcement := regexp.MustCompile(`http://(127\.0\.0\.1:[0-9]+)`)
	deadline, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	tick := time.NewTicker(50 * time.Millisecond)
	defer tick.Stop()
	for {
		if exited, _ := child.Exited(0); exited {
			if client != nil {
				client.Close()
			}
			_ = child.Close()
			return nil, errors.New("owned OpenCode exited before ready")
		}
		if client == nil {
			file, err := os.Open(child.StdoutPath)
			if err == nil {
				data := make([]byte, 64<<10)
				n, _ := file.Read(data)
				_ = file.Close()
				match := announcement.FindSubmatch(data[:n])
				if len(match) == 2 {
					client, err = Connect(string(match[1]), password, canonical)
					if err != nil {
						_ = child.Close()
						return nil, err
					}
				}
			}
		}
		if client != nil {
			probe, stop := context.WithTimeout(deadline, 300*time.Millisecond)
			_, err := client.call(probe, "GET", "/api/info", nil)
			stop()
			if err == nil {
				return &Process{Client: client, owned: child}, nil
			}
		}
		select {
		case <-deadline.Done():
			if client != nil {
				client.Close()
			}
			_ = child.Close()
			return nil, errors.New("owned OpenCode readiness timed out")
		case <-tick.C:
		}
	}
}
