package backend

import (
	"context"
	"errors"
	"net/http"
	"net/url"
	"path/filepath"
	"time"

	"fintwind-go/internal/backendcommand"
	"fintwind-go/internal/backendmanagement"
	"fintwind-go/internal/backendstore"
)

func (s *Server) checkOpenCodeUpdate(ctx context.Context) (backendstore.Object, error) {
	var path, current, latest, checkError any
	binary, err := s.providerBinary("")
	if err == nil {
		path = binary
		raw, e := backendcommand.Run(ctx, binary, []string{"--version"}, filepath.Dir(s.Directory), s.WorkspaceRoot, nil, 5*time.Second)
		if e == nil {
			if parsed := backendmanagement.ParseCLI(raw); parsed != "" {
				current = parsed
			}
		}
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	defer transport.CloseIdleConnections()
	s.fileMu.Lock()
	settings, err := readObject(s.settingsPath)
	s.fileMu.Unlock()
	if err != nil {
		return nil, err
	}
	if proxy := settings.String("network_proxy_url"); proxy != "" {
		parsed, err := url.Parse(proxy)
		if err != nil || parsed.Host == "" || (parsed.Scheme != "http" && parsed.Scheme != "https" && parsed.Scheme != "socks5" && parsed.Scheme != "socks5h") {
			checkError = "invalid configured network proxy"
		} else {
			transport.Proxy = http.ProxyURL(parsed)
		}
	}
	available := false
	if checkError == nil {
		client := &http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error { return errors.New("update redirects refused") }}
		version, err := backendmanagement.Latest(ctx, client, backendmanagement.LatestURL)
		if err != nil {
			checkError = err.Error()
		} else {
			latest = version
			if before, ok := current.(string); ok {
				comparison, err := backendmanagement.Compare(version, before)
				if err != nil {
					checkError = err.Error()
				} else {
					available = comparison > 0
				}
			}
		}
	}
	return backendstore.Reply("openCodeUpdateStatus", backendstore.Fields(map[string]any{"path": path, "currentVersion": current, "latestVersion": latest, "updateAvailable": available, "checkError": checkError})), nil
}
