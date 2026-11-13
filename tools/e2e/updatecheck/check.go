package main

// Harness + Latest-over-real-socket table + semver ordering evidence.
// No _test.go files: everything runs inside this E2E binary against the real
// implementation and leaves a repeatable JSON artifact.

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/backendmanagement"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
)

type report struct {
	DaemonSHA256 string            `json:"daemonSha256"`
	Checks       map[string]bool   `json:"checks"`
	Notes        map[string]string `json:"notes"`
	Covered      []string          `json:"covered"`
	Uncovered    []string          `json:"uncovered"`
	Blocked      []string          `json:"blocked"`
	Passed       bool              `json:"passed"`
	Error        string            `json:"error,omitempty"`
	Command      string            `json:"command"`
}

type harness struct {
	ctx       context.Context
	round     string
	dataDir   string
	token     string
	daemonBin string
	self      string
	child     *ownedprocess.Owned
	client    *daemon.Client
	checks    map[string]bool
	notes     map[string]string
	blocked   []string
}

func run(ctx context.Context, daemonBin string) (err error) {
	if !filepath.IsAbs(daemonBin) {
		return errors.New("absolute daemon path required")
	}
	raw, err := os.ReadFile(daemonBin)
	if err != nil {
		return errors.New("daemon binary is not readable")
	}
	sum := sha256.Sum256(raw)
	out := report{DaemonSHA256: hex.EncodeToString(sum[:]), Checks: map[string]bool{}, Notes: map[string]string{}, Command: "updatecheck --daemon " + daemonBin}
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, round: round, dataDir: filepath.Join(round, "data"), token: uuid.NewString(), daemonBin: daemonBin, self: self, checks: out.Checks, notes: out.Notes}
	defer func() {
		out.Passed = err == nil
		if err != nil {
			out.Error = err.Error()
		}
		out.Blocked = h.blocked
		out.Covered = []string{
			"Latest over real loopback sockets: status/timeout/redirect/budget/JSON/semver/major checks",
			"semver prerelease ordering, build metadata ignored, numeric prerelease leading-zero refused",
			"daemon WS checkOpenCodeUpdate: refusing-proxy and invalid-proxy failures keep path/current, latest null, available false, checkError set, config untouched",
			"missing binary and failing --version keep partial identity without install or config rewrite",
			"real-network official latest GET attempted once read-only (see blocked when unreachable)",
		}
		out.Uncovered = []string{
			"daemon WS full-success wire unless the sandbox reaches the official endpoint (recorded under blocked when unreachable)",
			"download/install/upgrade paths: explicitly out of scope, never executed",
		}
		data, marshalErr := json.MarshalIndent(out, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "update-result.json"), data, 0o600)
		}
		if marshalErr != nil {
			err = errors.Join(err, marshalErr)
		}
		fmt.Println("[REPORT]", filepath.Join(round, "update-result.json"))
	}()
	if err := os.MkdirAll(h.dataDir, 0o700); err != nil {
		return err
	}
	if err := h.latestSuite(); err != nil {
		return err
	}
	if err := h.compareSuite(); err != nil {
		return err
	}
	if err := h.startDaemon(); err != nil {
		return err
	}
	defer func() { err = errors.Join(err, h.closeDaemon()) }()
	if err := h.daemonSuite(); err != nil {
		return err
	}
	if err := h.realNetworkSuite(); err != nil {
		return err
	}
	return nil
}

func (h *harness) writeDaemonJSON(content map[string]string) error {
	raw, err := json.Marshal(content)
	if err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(h.dataDir, "daemon.json"), raw, 0o600)
}

func (h *harness) daemonJSONUnchanged(want map[string]string) error {
	after, err := os.ReadFile(filepath.Join(h.dataDir, "daemon.json"))
	if err != nil {
		return err
	}
	var got map[string]string
	if err := json.Unmarshal(after, &got); err != nil {
		return err
	}
	if len(want) != len(got) {
		return errors.New("daemon configuration was rewritten")
	}
	for key, value := range want {
		if got[key] != value {
			return errors.New("daemon configuration was rewritten")
		}
	}
	return nil
}

func (h *harness) startDaemon() error {
	logs := filepath.Join(h.round, "first")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(h.daemonBin, []string{"--data-dir", h.dataDir, "--bind", "127.0.0.1:0"}, logs, h.round, ownedprocess.BuildEnv(h.round, map[string]string{"FINTWIND_DAEMON_TOKEN": h.token}))
	if err != nil {
		return err
	}
	address, err := waitDaemonReady(h.ctx, child)
	if err != nil {
		_ = child.Close()
		return err
	}
	client, err := daemon.Dial(h.ctx, address, h.token)
	if err != nil {
		_ = child.Close()
		return err
	}
	h.child, h.client = child, client
	return nil
}

func (h *harness) closeDaemon() error {
	if h.client != nil {
		h.client.Close()
		h.client = nil
	}
	if h.child != nil {
		err := h.child.Close()
		h.child = nil
		return err
	}
	return nil
}

func waitDaemonReady(ctx context.Context, child *ownedprocess.Owned) (string, error) {
	timer := time.NewTimer(15 * time.Second)
	defer timer.Stop()
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		data, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
		}
		if json.Unmarshal(data, &ready) == nil && ready.Address != "" {
			return ready.Address, nil
		}
		if done, err := child.Exited(0); err != nil || done {
			return "", errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-timer.C:
			return "", errors.New("ready deadline exceeded")
		case <-tick.C:
		}
	}
}

func (h *harness) checkUpdate() (backendstore.Object, error) {
	raw, err := h.client.Request(h.ctx, map[string]any{"type": "checkOpenCodeUpdate"})
	if err != nil {
		return nil, err
	}
	return backendstore.Decode(raw)
}

func failIf(err error, contains string) error {
	if err == nil {
		return errors.New("expected failure containing " + contains + " but the call succeeded")
	}
	if !strings.Contains(err.Error(), contains) {
		return fmt.Errorf("expected error containing %q, got %q", contains, err.Error())
	}
	return nil
}

// refusingProxyURL reserves a loopback port and closes it, so dialing it is
// deterministically refused without touching any external network.
func refusingProxyURL() (string, error) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return "", err
	}
	address := listener.Addr().String()
	_ = listener.Close()
	return "http://" + address, nil
}

// latestSuite drives backendmanagement.Latest over real loopback sockets.
// httptest servers are real TCP listeners; the client is a real http.Client.
func (h *harness) latestSuite() error {
	serve := func(status int, body string, sleep time.Duration) *httptest.Server {
		return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if sleep > 0 {
				select {
				case <-r.Context().Done():
					return
				case <-time.After(sleep):
				}
			}
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(status)
			_, _ = io.WriteString(w, body)
		}))
	}
	realClient := &http.Client{}
	cases := []struct {
		name    string
		handler func() *httptest.Server
		client  *http.Client
		timeout time.Duration
		want    string
		wantErr string
	}{
		{"ok", func() *httptest.Server { return serve(200, `{"version":"2.5.0"}`, 0) }, realClient, 0, "2.5.0", ""},
		{"prerelease", func() *httptest.Server { return serve(200, `{"version":"2.0.0-alpha"}`, 0) }, realClient, 0, "2.0.0-alpha", ""},
		{"build-metadata", func() *httptest.Server { return serve(200, `{"version":"2.0.0+build.1"}`, 0) }, realClient, 0, "2.0.0+build.1", ""},
		{"leading-zero-prerelease", func() *httptest.Server { return serve(200, `{"version":"2.0.0-01"}`, 0) }, realClient, 0, "", "invalid or unsupported"},
		{"major-below-2", func() *httptest.Server { return serve(200, `{"version":"1.99.0"}`, 0) }, realClient, 0, "", "invalid or unsupported"},
		{"invalid-semver", func() *httptest.Server { return serve(200, `{"version":"not-a-version"}`, 0) }, realClient, 0, "", "invalid or unsupported"},
		{"missing-version", func() *httptest.Server { return serve(200, `{}`, 0) }, realClient, 0, "", "invalid or unsupported"},
		{"empty-version", func() *httptest.Server { return serve(200, `{"version":""}`, 0) }, realClient, 0, "", "invalid or unsupported"},
		{"bad-json", func() *httptest.Server { return serve(200, `{"version":`, 0) }, realClient, 0, "", "invalid or unsupported"},
		{"non-200", func() *httptest.Server { return serve(500, `{"version":"2.5.0"}`, 0) }, realClient, 0, "", "refused"},
		{"over-budget", func() *httptest.Server {
			return serve(200, `{"version":"2.5.0","pad":"`+strings.Repeat("x", (1<<20)+8)+`"}`, 0)
		}, realClient, 0, "", "exceeds budget"},
		{"timeout", func() *httptest.Server { return serve(200, `{"version":"2.5.0"}`, 2*time.Second) }, realClient, 300 * time.Millisecond, "", "unavailable"},
	}
	for _, tc := range cases {
		server := tc.handler()
		err := func() error {
			defer server.Close()
			ctx := h.ctx
			if tc.timeout > 0 {
				var cancel context.CancelFunc
				ctx, cancel = context.WithTimeout(ctx, tc.timeout)
				defer cancel()
			}
			got, err := backendmanagement.Latest(ctx, tc.client, server.URL)
			if tc.wantErr == "" {
				if err != nil {
					return fmt.Errorf("%s: %w", tc.name, err)
				}
				if got != tc.want {
					return fmt.Errorf("%s: got %q want %q", tc.name, got, tc.want)
				}
				return nil
			}
			return failIf(err, tc.wantErr)
		}()
		if err != nil {
			return err
		}
		h.notes["latest-"+tc.name] = "covered"
	}
	redirectTarget := serve(200, `{"version":"2.5.0"}`, 0)
	defer redirectTarget.Close()
	redirector := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, redirectTarget.URL, http.StatusFound)
	}))
	defer redirector.Close()
	refusing := &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return errors.New("update redirects refused") }}
	if _, err := backendmanagement.Latest(h.ctx, refusing, redirector.URL); err == nil {
		return errors.New("redirect was followed")
	}
	h.notes["latest-redirect"] = "refused"
	h.checks["latest-real-socket-table"] = true
	return nil
}

func (h *harness) compareSuite() error {
	equal := []struct{ a, b string }{
		{"2.0.0", "2.0.0"},
		{"2.0.0+build.1", "2.0.0"},
		{"2.0.0+build.1", "2.0.0+other.2"},
	}
	for _, tc := range equal {
		n, err := backendmanagement.Compare(tc.a, tc.b)
		if err != nil || n != 0 {
			return fmt.Errorf("compare %s vs %s = %d, %v; want 0", tc.a, tc.b, n, err)
		}
	}
	ordered := []struct{ older, newer string }{
		{"2.0.0", "2.0.1"},
		{"2.0.0", "2.1.0"},
		{"2.0.0", "3.0.0"},
		{"1.9.0", "1.10.0"},
		{"2.0.0-alpha", "2.0.0"},
		{"2.0.0-alpha", "2.0.0-alpha.1"},
		{"2.0.0-alpha.1", "2.0.0-beta"},
		{"2.0.0-1", "2.0.0-alpha"},
	}
	for _, tc := range ordered {
		n, err := backendmanagement.Compare(tc.newer, tc.older)
		if err != nil || n <= 0 {
			return fmt.Errorf("compare %s vs %s = %d, %v; want >0", tc.newer, tc.older, n, err)
		}
		m, err := backendmanagement.Compare(tc.older, tc.newer)
		if err != nil || m >= 0 {
			return fmt.Errorf("compare %s vs %s = %d, %v; want <0", tc.older, tc.newer, m, err)
		}
	}
	for _, bad := range []string{"2.0.0-01", "1.0", "v2", strings.Repeat("9", 300)} {
		if _, err := backendmanagement.Compare(bad, "2.0.0"); err == nil {
			return fmt.Errorf("compare accepted %q", bad)
		}
		if _, err := backendmanagement.Compare("2.0.0", bad); err == nil {
			return fmt.Errorf("compare accepted %q on the right", bad)
		}
	}
	if !backendmanagement.SupportedVersion("2.0.0") || backendmanagement.SupportedVersion("1.99.9") {
		return errors.New("SupportedVersion boundary wrong")
	}
	h.checks["semver-ordering-evidence"] = true
	return nil
}
