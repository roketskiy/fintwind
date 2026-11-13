package backend

import (
	"bytes"
	"context"
	"errors"
	"net/http"
	"net/url"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendcommand"
	"fintwind-go/internal/backendmanagement"
	"fintwind-go/internal/backendstore"
)

func managementProxy(settings backendstore.Object) (map[string]string, error) {
	extra := map[string]string{}
	if proxy := settings.String("network_proxy_url"); proxy != "" {
		parsed, err := url.Parse(proxy)
		if err != nil || parsed.Host == "" || (parsed.Scheme != "http" && parsed.Scheme != "https" && parsed.Scheme != "socks5" && parsed.Scheme != "socks5h") {
			return nil, errors.New("invalid configured network proxy")
		}
		extra["HTTP_PROXY"] = proxy
		extra["HTTPS_PROXY"] = proxy
		extra["NO_PROXY"] = "localhost,127.0.0.1,::1"
	}
	return extra, nil
}

func (s *Server) manageOpenCode(ctx context.Context, c backendstore.Object) (backendstore.Object, error) {
	if s.runtime.HasActive() {
		return nil, errors.New("close task runtimes before managing OpenCode")
	}
	s.fileMu.Lock()
	settings, err := readObject(s.settingsPath)
	s.fileMu.Unlock()
	if err != nil {
		return nil, err
	}
	before := append([]byte(nil), settings["opencode_binary_path"]...)
	install := c.String("type") == "installOpenCode"
	current, currentErr := s.providerBinary("")
	if install {
		if currentErr == nil {
			return nil, errors.New("OpenCode is already installed; select update instead")
		}
		if _, err := exec.LookPath("opencode"); err == nil {
			return nil, errors.New("an existing OpenCode on PATH must not be overwritten")
		}
	} else {
		if currentErr != nil {
			return nil, errors.New("selected OpenCode executable unavailable")
		}
		requested, err := filepath.EvalSymlinks(c.String("path"))
		if err != nil {
			return nil, errors.New("update target unavailable")
		}
		selected, err := filepath.EvalSymlinks(current)
		if err != nil || !strings.EqualFold(requested, selected) || !backendmanagement.Owns(s.Directory, current) {
			return nil, errors.New("only this isolated profile's managed OpenCode can be updated")
		}
	}
	bun, err := exec.LookPath("bun")
	if err != nil {
		return nil, errors.New("Bun is required for profile-local OpenCode installation")
	}
	bun, err = filepath.Abs(bun)
	if err != nil {
		return nil, err
	}
	extra, err := managementProxy(settings)
	if err != nil {
		return nil, err
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	defer transport.CloseIdleConnections()
	if proxy := extra["HTTPS_PROXY"]; proxy != "" {
		parsed, _ := url.Parse(proxy)
		transport.Proxy = http.ProxyURL(parsed)
	}
	client := &http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error { return errors.New("update redirects refused") }}
	latest, err := backendmanagement.Latest(ctx, client, backendmanagement.LatestURL)
	if err != nil {
		return nil, err
	}
	run := func(ctx context.Context, binary string, args []string, environment map[string]string, timeout time.Duration) ([]byte, error) {
		return backendcommand.Run(ctx, binary, args, filepath.Dir(s.Directory), s.WorkspaceRoot, environment, timeout)
	}
	if !install {
		if c.String("version") != latest {
			return nil, errors.New("update version no longer matches the official latest version")
		}
		output, err := run(ctx, current, []string{"--version"}, nil, 5*time.Second)
		if err != nil {
			return nil, err
		}
		comparison, err := backendmanagement.Compare(latest, backendmanagement.ParseCLI(output))
		if err != nil || comparison <= 0 {
			return nil, errors.New("requested OpenCode version is not newer")
		}
	}
	// Clear our own inactive pool before touching executable files. Never stop
	// resident task actors, installed applications, or another session's jobs.
	if err := s.servicePool.reset(); err != nil {
		return nil, err
	}
	binary, err := backendmanagement.ManagedInstall(ctx, s.Directory, bun, latest, extra, run)
	if err != nil {
		return nil, errors.New("profile-local OpenCode installation failed; configuration unchanged")
	}
	s.fileMu.Lock()
	defer s.fileMu.Unlock()
	after, err := readObject(s.settingsPath)
	if err != nil {
		return nil, err
	}
	if !bytes.Equal(before, after["opencode_binary_path"]) {
		return nil, errors.New("OpenCode selection changed during installation; configuration preserved")
	}
	after.Set("opencode_binary_path", binary)
	if err := writeObject(s.settingsPath, after); err != nil {
		return nil, err
	}
	if install {
		return backendstore.Reply("openCodeInstalled", backendstore.Fields(map[string]any{"settings": after})), nil
	}
	return backendstore.Reply("ack", nil), nil
}
