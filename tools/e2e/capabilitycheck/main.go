// capabilitycheck：内置浏览器完整移除验收（独立 E2E，只新增本目录）。
//
// 失败场景（先写，后验证；只认 wire/HTTP/文件，不认源码阅读）：
//  1. protocol24-hello 必须被拒（type=rejected，随后断线）；同条件 protocol25
//     必须正常 hello（protocolVersion=25，非零 env/instance）。
//  2. 旧 browserList / browserInvoke（合法身份 requestId/session/runtime）必须
//     回 RPC-error 且文案含 unsupported；绝不能出现 status=ok 的成功结果；
//     同一连接随后 getSettings 必须正常（拒旧不伤新）。
//  3. 旧 browserPublish / browserHost / browserResult / browserCancel 帧不得
//     注册任何状态或触发执行：允许服务端 close 或 rejected，只要无动作、无成功
//     回包；随后新连接普通 RPC 正常即 daemon 健康。
//  4. GET /v1/browser-tools 不得升级 WS，必须 404。
//  5. 源码只读断言：Go 产品树（internal/、cmd/app/、cmd/daemon/）零 browser/
//     webview 字样；系统 OpenURL 调用点保留。
//     （静态断言只做存在性守卫，行为结论一律以 wire 为准。）
//  6. 隔离边界：无 provider 凭据；daemon 数据目录全在 Owned round 内；
//     updateSetting 写入只落本轮 daemon.json。
//
// 不做：其他业务回归、归档的 Rust 桌面实现、
// 真实模型/OAuth/视觉、用户 MCP 与 provider 原生能力的干预判定。

package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

type opRecord struct {
	Name   string `json:"name"`
	OK     bool   `json:"ok"`
	Detail string `json:"detail"`
}

type harness struct {
	ctx      context.Context
	checks   map[string]bool
	ops      []opRecord
	counts   map[string]any
	versions map[string]any
}

func (h *harness) note(name string, err error) {
	rec := opRecord{Name: name, OK: err == nil}
	if err != nil {
		rec.Detail = err.Error()
	}
	h.ops = append(h.ops, rec)
}

func (h *harness) pass(name string) { h.checks[name] = true }

type daemonInst struct {
	round   string
	dataDir string
	child   *ownedprocess.Owned
	addr    string
	token   string
}

func newToken() string {
	sum := sha256.Sum256([]byte(fmt.Sprint(time.Now().UnixNano())))
	return hex.EncodeToString(sum[:])
}

func (h *harness) launchDaemon(daemonBinary string) (*daemonInst, error) {
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return nil, err
	}
	dataDir := filepath.Join(round, "data")
	if err := os.MkdirAll(filepath.Join(round, "workspace", "main"), 0o700); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return nil, err
	}
	token := newToken()
	logs := filepath.Join(round, "daemon-logs")
	if err := os.MkdirAll(logs, 0o700); err != nil {
		return nil, err
	}
	env := ownedprocess.BuildEnv(round, map[string]string{"FINTWIND_DAEMON_TOKEN": token})
	child, err := ownedprocess.StartWithEnvironment(daemonBinary,
		[]string{"--data-dir", dataDir, "--bind", "127.0.0.1:0"}, logs, round, env)
	if err != nil {
		return nil, err
	}
	addr, err := waitReady(h.ctx, child)
	if err != nil {
		_ = child.Close()
		return nil, err
	}
	rel, err := filepath.Rel(round, dataDir)
	if err != nil || !filepath.IsLocal(rel) {
		_ = child.Close()
		return nil, fmt.Errorf("data dir escapes round: %s", dataDir)
	}
	return &daemonInst{round: round, dataDir: dataDir, child: child, addr: addr, token: token}, nil
}

func waitReady(ctx context.Context, child *ownedprocess.Owned) (string, error) {
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	for {
		raw, _ := os.ReadFile(child.StdoutPath)
		var ready struct {
			Address string `json:"address"`
			PID     uint32 `json:"pid"`
		}
		if json.Unmarshal(raw, &ready) == nil && ready.PID == child.PID && ready.Address != "" {
			return ready.Address, nil
		}
		if done, _ := child.Exited(0); done {
			return "", errors.New("daemon exited before ready")
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-tick.C:
		}
	}
}

// rawDial 用指定协议版本握手，返回首帧。
func rawDial(addr, token string, version int) (*websocket.Conn, map[string]any, error) {
	dialer := websocket.Dialer{HandshakeTimeout: 10 * time.Second}
	conn, _, err := dialer.Dial("ws://"+addr+"/v1", nil)
	if err != nil {
		return nil, nil, err
	}
	hello := map[string]any{"type": "hello", "protocolVersion": version, "token": token, "clientId": uuid.NewString(), "resumeFrom": []any{}}
	_ = conn.SetWriteDeadline(time.Now().Add(10 * time.Second))
	if err := conn.WriteJSON(hello); err != nil {
		_ = conn.Close()
		return nil, nil, err
	}
	_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	var frame map[string]any
	if err := conn.ReadJSON(&frame); err != nil {
		_ = conn.Close()
		return nil, nil, err
	}
	_ = conn.SetReadDeadline(time.Time{})
	return conn, frame, nil
}

func readFrame(conn *websocket.Conn, timeout time.Duration) (map[string]any, error) {
	_ = conn.SetReadDeadline(time.Now().Add(timeout))
	var frame map[string]any
	if err := conn.ReadJSON(&frame); err != nil {
		return nil, err
	}
	_ = conn.SetReadDeadline(time.Time{})
	return frame, nil
}

func main() {
	daemonBinary := flag.String("daemon", "", "absolute daemon binary")
	flag.Parse()
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Second)
	defer cancel()
	if err := run(ctx, *daemonBinary); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, daemonBinary string) (err error) {
	if !filepath.IsAbs(daemonBinary) {
		return errors.New("absolute daemon binary required")
	}
	for _, key := range []string{"DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "FINTWIND_AGENT_REAL_PROVIDER"} {
		if strings.TrimSpace(os.Getenv(key)) != "" {
			return fmt.Errorf("credential/marker %s must stay unset", key)
		}
	}
	sum, err := fileSHA(daemonBinary)
	if err != nil {
		return err
	}
	h := &harness{ctx: ctx, checks: map[string]bool{}, counts: map[string]any{}, versions: map[string]any{"protocolVersion": daemon.ProtocolVersion}}
	report := map[string]any{
		"command":      "capabilitycheck --daemon " + daemonBinary,
		"module":       "real Go daemon WS + raw frames + HTTP; no provider, no user browser, no model, no visual, no real OAuth",
		"provider":     "none",
		"realRequests": 0,
		"daemonSha256": sum,
		"daemonBinary": daemonBinary,
		"checks":       h.checks,
		"versions":     h.versions,
		"notes": map[string]string{
			"scope": "browser removal acceptance; business regression deferred to unified 22+ groups",
		},
		"uncovered": []string{
			"business RPC regression (saveTaskState/hydrate/scheduled/events): covered by later unified runs, not claimed here",
			"Archived Rust desktop implementation is outside the Go application checks",
			"real provider/model calls, real OAuth flows, visual checks: never performed",
			"user MCP and provider-native browser capabilities: out of scope, must not be treated as Fintwind built-in",
		},
	}
	firstRound := ""
	defer func() {
		report["ops"] = h.ops
		report["checks"] = h.checks
		report["counts"] = h.counts
		report["passed"] = err == nil
		if firstRound == "" {
			if round, rerr := ownedprocess.NewRoundDir(); rerr == nil {
				firstRound = round
			} else {
				fmt.Fprintln(os.Stderr, "[X] report dir unavailable")
				return
			}
		}
		out, _ := json.MarshalIndent(report, "", "  ")
		_ = os.WriteFile(filepath.Join(firstRound, "capability-result.json"), out, 0o600)
		fmt.Println("[REPORT]", filepath.Join(firstRound, "capability-result.json"))
	}()

	if err := roundStaticGuards(h, daemonBinary); err != nil {
		return err
	}
	inst, err := h.launchDaemon(daemonBinary)
	if err != nil {
		return err
	}
	firstRound = inst.round
	defer inst.child.Close()
	if err := roundProtocol(h, inst); err != nil {
		return err
	}
	if err := roundLegacyRPC(h, inst); err != nil {
		return err
	}
	if err := roundLegacyFrames(h, inst); err != nil {
		return err
	}
	if err := roundHTTPRoute(h, inst); err != nil {
		return err
	}
	if err := roundSettingsBoundary(h, inst); err != nil {
		return err
	}
	return nil
}

func fileSHA(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:]), nil
}

var _ = http.MethodGet
