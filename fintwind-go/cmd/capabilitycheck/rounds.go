package main

import (
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"

	"github.com/google/uuid"
)

func repoRoot(daemonBinary string) string {
	return filepath.Dir(filepath.Dir(filepath.Dir(daemonBinary)))
}

func roundStaticGuards(h *harness, daemonBinary string) error {
	root := repoRoot(daemonBinary)
	// 1. 产品树零内置 browser 标记（只扫产品目录，不含测试 harness 自身）。
	// 针收窄到精确标记：裸 "browser"/"webview" 子串会误伤保留项
	// （panes.go:17 注释声明无 webview；settings_mcp_store.go:795 系 MCP
	// OAuth 系统浏览器文案，属保留项），故只认 wire 类型/桥模块/host 依赖。
	scanDirs := []string{
		filepath.Join(root, "fintwind-go", "internal"),
		filepath.Join(root, "fintwind-go", "cmd", "app"),
		filepath.Join(root, "fintwind-go", "cmd", "daemon"),
	}
	needles := []string{
		"browserlist", "browserinvoke", "browserpublish", "browserhost",
		"browserresult", "browsercancel", "browserrequest", "browsersharerejected",
		"backendbrowser", "browser_broker", "browseraction", "browser_tools",
		"browser-tools", "webview2", "webview-com", "playwright",
	}
	var hits []string
	files := 0
	for _, dir := range scanDirs {
		err := filepath.Walk(dir, func(path string, info os.FileInfo, err error) error {
			if err != nil || info.IsDir() || !strings.HasSuffix(path, ".go") {
				return nil
			}
			files++
			raw, err := os.ReadFile(path)
			if err != nil {
				return nil
			}
			lower := strings.ToLower(string(raw))
			for _, needle := range needles {
				if strings.Contains(lower, needle) {
					hits = append(hits, path+":"+needle)
				}
			}
			return nil
		})
		if err != nil {
			h.note("src-no-browser-wire", err)
			return err
		}
	}
	h.counts["staticGoFiles"] = files
	h.note("src-no-browser-wire", nil)
	if len(hits) != 0 {
		err := fmt.Errorf("browser remnants: %v", hits)
		h.note("src-no-browser-wire", err)
		return err
	}
	h.pass("src-no-browser-wire")

	// 2. 系统 OpenURL 调用点保留（ Cargo/Rust 不动，只读 Go 调用点）。
	openURLSites := map[string]string{
		filepath.Join(root, "fintwind-go", "cmd", "app", "chrome.go"):          "OpenURL",
		filepath.Join(root, "fintwind-go", "cmd", "app", "settings_auth.go"):   "OpenURL",
		filepath.Join(root, "fintwind-go", "cmd", "app", "settings_market.go"): "OpenURL",
	}
	for path, needle := range openURLSites {
		raw, err := os.ReadFile(path)
		if err != nil || !strings.Contains(string(raw), needle) {
			h.note("src-openurl-kept", fmt.Errorf("missing %s in %s", needle, path))
			return fmt.Errorf("src-openurl-kept: %s", path)
		}
	}
	h.note("src-openurl-kept", nil)
	h.pass("src-openurl-kept")

	// 3. Rust MCP OAuth 保留（只读存在性，不真跑 OAuth）。
	oauthPath := filepath.Join(root, "crates", "fintwind-client", "src", "persistence.rs")
	raw, err := os.ReadFile(oauthPath)
	if err != nil || !strings.Contains(string(raw), "start_provider_oauth") {
		h.note("src-mcp-oauth-kept", fmt.Errorf("oauth entry missing"))
		return fmt.Errorf("src-mcp-oauth-kept")
	}
	h.note("src-mcp-oauth-kept", nil)
	h.pass("src-mcp-oauth-kept")

	// 4. 单一 /v1 路由，无 browser-tools。
	srvPath := filepath.Join(root, "fintwind-go", "internal", "backend", "server.go")
	srv, err := os.ReadFile(srvPath)
	if err != nil {
		h.note("src-single-route", err)
		return err
	}
	if !strings.Contains(string(srv), `"/v1"`) || strings.Contains(string(srv), "browser-tools") {
		err = fmt.Errorf("unexpected route table state")
		h.note("src-single-route", err)
		return err
	}
	h.note("src-single-route", nil)
	h.pass("src-single-route")
	return nil
}

func roundProtocol(h *harness, inst *daemonInst) error {
	c24, f24, err := rawDial(inst.addr, inst.token, 24)
	h.note("proto24-rejected", err)
	if err != nil {
		return fmt.Errorf("proto24 dial: %w", err)
	}
	if typ, _ := f24["type"].(string); typ != "rejected" {
		_ = c24.Close()
		err = fmt.Errorf("proto24: want rejected, got %v", f24["type"])
		h.note("proto24-rejected", err)
		return err
	}
	h.pass("proto24-rejected")
	if _, err := readFrame(c24, 10*time.Second); err == nil {
		_ = c24.Close()
		err = fmt.Errorf("proto24: connection stayed open after reject")
		h.note("proto24-closed", err)
		return err
	}
	_ = c24.Close()
	h.note("proto24-closed", nil)
	h.pass("proto24-closed")

	c25, f25, err := rawDial(inst.addr, inst.token, 25)
	h.note("proto25-hello", err)
	if err != nil {
		return fmt.Errorf("proto25 dial: %w", err)
	}
	defer c25.Close()
	ver, _ := f25["protocolVersion"].(float64)
	env, _ := f25["environmentId"].(string)
	ins, _ := f25["instanceId"].(string)
	if int(ver) != 25 {
		return fmt.Errorf("proto25: version %v", f25["protocolVersion"])
	}
	for _, id := range []string{env, ins} {
		parsed, perr := uuid.Parse(id)
		if perr != nil || parsed == uuid.Nil {
			return fmt.Errorf("proto25: bad identity %q", id)
		}
	}
	h.pass("proto25-hello")
	h.versions["daemonEnv"] = env
	h.versions["daemonInstance"] = ins
	return nil
}

func awaitResponse(conn interface {
	ReadJSON(v any) error
	SetReadDeadline(t time.Time) error
}, wantID string, timeout time.Duration) (map[string]any, error) {
	deadline := time.Now().Add(timeout)
	for {
		rest := time.Until(deadline)
		if rest <= 0 {
			return nil, fmt.Errorf("response %s timeout", wantID)
		}
		_ = conn.SetReadDeadline(time.Now().Add(rest))
		var frame map[string]any
		if err := conn.ReadJSON(&frame); err != nil {
			return nil, err
		}
		if frame["type"] == "response" && frame["requestId"] == wantID {
			return frame, nil
		}
	}
}

func outcomeOf(frame map[string]any) (status, message, payload string) {
	oc, _ := frame["outcome"].(map[string]any)
	status, _ = oc["status"].(string)
	if em, _ := oc["error"].(map[string]any); em != nil {
		message, _ = em["message"].(string)
	}
	if pl, _ := oc["payload"]; pl != nil {
		payload = fmt.Sprintf("%v", pl)
	}
	return status, message, payload
}

func sendRequest(conn interface {
	WriteJSON(v any) error
	SetWriteDeadline(t time.Time) error
}, rpc string, command map[string]any) error {
	_ = conn.SetWriteDeadline(time.Now().Add(10 * time.Second))
	return conn.WriteJSON(map[string]any{
		"type": "request", "requestId": rpc,
		"sessionId": uuid.NewString(), "runtimeId": uuid.NewString(),
		"command": command,
	})
}

func roundLegacyRPC(h *harness, inst *daemonInst) error {
	conn, _, err := rawDial(inst.addr, inst.token, 25)
	if err != nil {
		return err
	}
	defer conn.Close()

	rpc := uuid.NewString()
	if err := sendRequest(conn, rpc, map[string]any{"type": "browserList"}); err != nil {
		return err
	}
	frame, err := awaitResponse(conn, rpc, 15*time.Second)
	h.note("legacy-list-unsupported", err)
	if err != nil {
		return err
	}
	status, message, _ := outcomeOf(frame)
	if status != "error" || !strings.Contains(message, "unsupported") {
		err = fmt.Errorf("legacy-list: status=%q msg=%q", status, message)
		h.note("legacy-list-unsupported", err)
		return err
	}
	h.pass("legacy-list-unsupported")

	rpc = uuid.NewString()
	scope := map[string]any{"sessionId": uuid.NewString(), "runtimeId": uuid.NewString(), "pageId": uuid.NewString(), "grantId": uuid.NewString()}
	if err := sendRequest(conn, rpc, map[string]any{"type": "browserInvoke", "scope": scope, "action": map[string]any{"kind": "snapshot"}}); err != nil {
		return err
	}
	frame, err = awaitResponse(conn, rpc, 15*time.Second)
	h.note("legacy-invoke-unsupported", err)
	if err != nil {
		return err
	}
	status, message, _ = outcomeOf(frame)
	if status != "error" || !strings.Contains(message, "unsupported") {
		err = fmt.Errorf("legacy-invoke: status=%q msg=%q", status, message)
		h.note("legacy-invoke-unsupported", err)
		return err
	}
	h.pass("legacy-invoke-unsupported")
	h.note("legacy-invoke-no-success", nil)
	h.pass("legacy-invoke-no-success")

	rpc = uuid.NewString()
	if err := sendRequest(conn, rpc, map[string]any{"type": "getSettings"}); err != nil {
		return err
	}
	frame, err = awaitResponse(conn, rpc, 15*time.Second)
	h.note("normal-rpc-after-reject", err)
	if err != nil {
		return err
	}
	if status, _, _ := outcomeOf(frame); status != "ok" {
		err = fmt.Errorf("getSettings after legacy reject: %v", frame["outcome"])
		h.note("normal-rpc-after-reject", err)
		return err
	}
	h.pass("normal-rpc-after-reject")
	return nil
}

func expectNoAction(h *harness, inst *daemonInst, name string, frame map[string]any) error {
	var err error
	defer func() {
		h.note(name, err)
		if err == nil {
			h.pass(name)
		}
	}()
	conn, _, derr := rawDial(inst.addr, inst.token, 25)
	if derr != nil {
		return derr
	}
	defer conn.Close()
	_ = conn.SetWriteDeadline(time.Now().Add(10 * time.Second))
	if serr := conn.WriteJSON(frame); serr != nil {
		return serr
	}
	got, rerr := readFrame(conn, 10*time.Second)
	if rerr == nil {
		return fmt.Errorf("unexpected live frame after %s: type=%v", name, got["type"])
	}
	if strings.Contains(rerr.Error(), "i/o timeout") {
		return fmt.Errorf("connection stayed open with no reject after %s", name)
	}
	return nil
}

func roundLegacyFrames(h *harness, inst *daemonInst) error {
	scope := map[string]any{"sessionId": uuid.NewString(), "runtimeId": uuid.NewString(), "pageId": uuid.NewString(), "grantId": uuid.NewString()}
	frames := []struct {
		name  string
		frame map[string]any
	}{
		{"legacy-publish-noop", map[string]any{"type": "browserPublish", "pages": []any{map[string]any{"scope": scope, "url": "https://x.example/", "title": "X"}}}},
		{"legacy-host-noop", map[string]any{"type": "browserHost", "scope": scope}},
		{"legacy-result-noop", map[string]any{"type": "browserResult", "requestId": uuid.NewString(), "result": map[string]any{"kind": "ok", "value": map[string]any{}}}},
		{"legacy-cancel-noop", map[string]any{"type": "browserCancel", "requestId": uuid.NewString()}},
	}
	for _, f := range frames {
		if err := expectNoAction(h, inst, f.name, f.frame); err != nil {
			return err
		}
	}
	conn, _, err := rawDial(inst.addr, inst.token, 25)
	if err != nil {
		return err
	}
	defer conn.Close()
	rpc := uuid.NewString()
	if err := sendRequest(conn, rpc, map[string]any{"type": "getSettings"}); err != nil {
		return err
	}
	frame, err := awaitResponse(conn, rpc, 15*time.Second)
	h.note("daemon-healthy-after-frames", err)
	if err != nil {
		return err
	}
	if status, _, _ := outcomeOf(frame); status != "ok" {
		err = fmt.Errorf("daemon unhealthy: %v", frame["outcome"])
		h.note("daemon-healthy-after-frames", err)
		return err
	}
	h.pass("daemon-healthy-after-frames")
	return nil
}

func roundHTTPRoute(h *harness, inst *daemonInst) error {
	client := &http.Client{Timeout: 15 * time.Second}
	resp, err := client.Get("http://" + inst.addr + "/v1/browser-tools")
	h.note("http-browsetools-404", err)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	h.counts["browserToolsStatus"] = resp.StatusCode
	if resp.StatusCode != http.StatusNotFound {
		err = fmt.Errorf("want 404, got %d", resp.StatusCode)
		h.note("http-browsetools-404", err)
		return err
	}
	h.pass("http-browsetools-404")
	return nil
}

func roundSettingsBoundary(h *harness, inst *daemonInst) error {
	client, err := daemon.Dial(h.ctx, inst.addr, inst.token)
	h.note("client25-dial", err)
	if err != nil {
		return err
	}
	defer client.Close()
	h.pass("client25-dial")

	raw, err := client.Request(h.ctx, map[string]any{"type": "getSettings"})
	h.note("client-getsettings", err)
	if err != nil {
		return err
	}
	_ = raw
	h.pass("client-getsettings")

	const probe = "http://127.0.0.1:9/"
	raw, err = client.Request(h.ctx, map[string]any{"type": "updateSetting", "key": "network_proxy_url", "value": probe})
	h.note("settings-write-ok", err)
	if err != nil {
		return err
	}
	h.pass("settings-write-ok")

	content, err := os.ReadFile(filepath.Join(inst.dataDir, "daemon.json"))
	h.note("write-lands-in-round", err)
	if err != nil {
		return err
	}
	if !strings.Contains(string(content), "127.0.0.1:9") {
		err = fmt.Errorf("probe value not in round daemon.json")
		h.note("write-lands-in-round", err)
		return err
	}
	h.pass("write-lands-in-round")
	h.counts["dataDir"] = inst.dataDir
	return nil
}
