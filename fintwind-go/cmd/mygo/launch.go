package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/launchprofile"
	"fintwind-go/internal/ownedprocess"
)

type daemonReady struct {
	Address  string `json:"address"`
	Protocol int    `json:"protocolVersion"`
	PID      uint32 `json:"pid"`
}

func launch(ctx context.Context, plan launchprofile.Plan, binaries binaries, runDir string) error {
	tokenBytes := make([]byte, 32)
	if _, err := rand.Read(tokenBytes); err != nil {
		return err
	}
	token := hex.EncodeToString(tokenBytes)
	daemonDir := filepath.Join(runDir, "daemon")
	appDir := filepath.Join(runDir, "app")
	for _, dir := range []string{daemonDir, appDir} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
	}
	args := []string{"--bind", "127.0.0.1:0", "--parent-pid", strconv.Itoa(os.Getpid())}
	args = append(args, "--data-dir", plan.DataDir)
	backend, err := ownedprocess.StartWithEnvironment(binaries.daemon, args, daemonDir, plan.WorkDir, withEnvironment(plan.Environment, map[string]string{"FINTWIND_DAEMON_TOKEN": token, "FINTWIND_MYGO_EXPECTED_DATA_DIR": plan.DataDir}))
	if err != nil {
		return err
	}
	defer backend.Close()
	startupTimeout := 25 * time.Second
	if plan.Profile == launchprofile.Daily {
		fmt.Println("[OK] Opening existing Fintwind data directly.")
		startupTimeout = 3 * time.Minute
	}
	ready, err := waitReady(ctx, backend, startupTimeout)
	if err != nil {
		return err
	}
	if ready.Protocol != daemon.ProtocolVersion || ready.PID != backend.PID {
		return errors.New("daemon version or owned-process identity mismatch; frontend was not opened")
	}
	connectCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	client, err := daemon.Dial(connectCtx, ready.Address, token)
	cancel()
	if err != nil {
		return fmt.Errorf("daemon handshake failed; frontend was not opened: %w", err)
	}
	defer client.Close()
	if probeSession != "" {
		checkCtx, checkCancel := context.WithTimeout(ctx, 90*time.Second)
		data, checkErr := client.Request(checkCtx, map[string]any{"type": "hydrateSession", "sessionId": probeSession})
		checkCancel()
		result := map[string]any{"passed": false}
		mcpCtx, mcpCancel := context.WithTimeout(ctx, 15*time.Second)
		mcpData, mcpErr := client.Request(mcpCtx, map[string]any{"type": "loadMcpServers"})
		mcpCancel()
		if mcpErr != nil {
			result["mcpError"] = mcpErr.Error()
		} else {
			var mcp struct {
				Servers []json.RawMessage `json:"servers"`
			}
			if err := json.Unmarshal(mcpData, &mcp); err != nil || mcp.Servers == nil {
				result["mcpError"] = "invalid server list"
			} else {
				result["mcpServers"] = len(mcp.Servers)
			}
		}
		if checkErr != nil {
			result["error"] = checkErr.Error()
		} else {
			var detail struct {
				Session *struct {
					ID         string            `json:"id"`
					ProjectID  string            `json:"project_id"`
					NativeID   *string           `json:"native_session_id"`
					Messages   []json.RawMessage `json:"messages"`
					Blocks     []json.RawMessage `json:"transcript_blocks"`
					Background []json.RawMessage `json:"background_work"`
				} `json:"session"`
			}
			if err := json.Unmarshal(data, &detail); err != nil {
				result["error"] = err.Error()
			} else if detail.Session != nil && detail.Session.ID == probeSession {
				result["passed"] = true
				result["bytes"] = len(data)
				result["messages"] = len(detail.Session.Messages)
				result["transcriptBlocks"] = len(detail.Session.Blocks)
				result["backgroundWork"] = len(detail.Session.Background)
				if detail.Session.NativeID != nil && len(detail.Session.Messages) == 0 {
					readCtx, done := context.WithTimeout(ctx, 90*time.Second)
					catalog, e := client.Request(readCtx, map[string]any{"type": "loadTaskState"})
					var state struct {
						Projects []struct {
							ID   string `json:"id"`
							Path string `json:"path"`
						} `json:"projects"`
					}
					if e == nil {
						e = json.Unmarshal(catalog, &state)
					}
					var directory string
					for _, project := range state.Projects {
						if project.ID == detail.Session.ProjectID {
							directory = project.Path
							break
						}
					}
					if e == nil && directory != "" {
						var native []byte
						native, e = client.Request(readCtx, map[string]any{"type": "fetchNativeTranscript", "directory": directory, "sessionId": *detail.Session.NativeID})
						if e == nil {
							var transcript struct {
								Transcript struct {
									Messages []json.RawMessage `json:"messages"`
									Blocks   []json.RawMessage `json:"blocks"`
									Turns    []json.RawMessage `json:"turns"`
								} `json:"transcript"`
							}
							e = json.Unmarshal(native, &transcript)
							if e == nil {
								result["nativeMessages"] = len(transcript.Transcript.Messages)
								result["nativeBlocks"] = len(transcript.Transcript.Blocks)
								result["nativeTurns"] = len(transcript.Transcript.Turns)
							}
						}
						if e == nil {
							usage, usageErr := client.Request(readCtx, map[string]any{"type": "fetchNativeUsage", "directory": directory, "sessionId": *detail.Session.NativeID})
							var parsed struct {
								Usage struct {
									Tokens   uint64  `json:"tokens"`
									Measured bool    `json:"measured"`
									Total    *uint64 `json:"total_tokens"`
								} `json:"usage"`
							}
							if usageErr != nil {
								result["usageError"] = usageErr.Error()
							} else if json.Unmarshal(usage, &parsed) != nil {
								result["usageError"] = "invalid usage"
							} else {
								result["usageMeasured"] = parsed.Usage.Measured
								result["usageTokens"] = parsed.Usage.Tokens
								result["usageTotal"] = parsed.Usage.Total
							}
						}
					} else if e == nil {
						e = errors.New("native session project not found")
					}
					done()
					if e != nil {
						result["nativeError"] = e.Error()
					}
				}
			}
		}
		if data, err := json.Marshal(result); err == nil {
			_ = os.WriteFile(filepath.Join(runDir, "session-probe.json"), data, 0o600)
		}
	}
	if probeProvider && plan.Profile == launchprofile.Daily && selectedBackend == "go" {
		probeCtx, stop := context.WithTimeout(ctx, 60*time.Second)
		raw, probeErr := client.Request(probeCtx, map[string]any{"type": "probeProvider", "discoverModels": true, "probeVersion": true})
		stop()
		result := map[string]any{"passed": false}
		if probeErr == nil {
			var response struct {
				Probe struct {
					Installed bool              `json:"installed"`
					Models    []json.RawMessage `json:"models"`
				} `json:"probe"`
				Version any `json:"version"`
			}
			if json.Unmarshal(raw, &response) == nil {
				result["passed"] = response.Probe.Installed
				result["models"] = len(response.Probe.Models)
				result["version"] = response.Version
			}
		} else {
			result["error"] = probeErr.Error()
		}
		checkCtx, checkCancel := context.WithTimeout(ctx, 45*time.Second)
		catalog, catalogErr := client.Request(checkCtx, map[string]any{"type": "loadTaskState"})
		if catalogErr == nil {
			var state struct {
				Projects []struct {
					ID   string `json:"id"`
					Path string `json:"path"`
				} `json:"projects"`
				Sessions []struct {
					ID        string `json:"id"`
					ProjectID string `json:"project_id"`
				} `json:"sessions"`
			}
			if json.Unmarshal(catalog, &state) == nil {
				volumes := map[string]bool{}
				workspaceOK, workspaceFailed, hydrated, hydrateFailed := 0, 0, 0, 0
				for _, project := range state.Projects {
					volume := strings.ToUpper(filepath.VolumeName(project.Path))
					if volume == "" || volumes[volume] {
						continue
					}
					volumes[volume] = true
					_, err := client.Request(checkCtx, map[string]any{"type": "workspace", "operation": map[string]any{"type": "listProjectFiles", "root": project.Path, "cap": 1}})
					if err != nil {
						workspaceFailed++
					} else {
						workspaceOK++
					}
					for _, session := range state.Sessions {
						if session.ProjectID != project.ID {
							continue
						}
						data, err := client.Request(checkCtx, map[string]any{"type": "hydrateSession", "sessionId": session.ID})
						var detail struct {
							Session json.RawMessage `json:"session"`
						}
						if err != nil || json.Unmarshal(data, &detail) != nil || len(detail.Session) == 0 || string(detail.Session) == "null" {
							hydrateFailed++
						} else {
							hydrated++
						}
						break
					}
				}
				result["projectVolumesChecked"], result["projectVolumesFailed"] = workspaceOK, workspaceFailed
				result["sessionsHydrated"], result["sessionHydrationFailed"] = hydrated, hydrateFailed
			}
		} else {
			result["catalogError"] = catalogErr.Error()
		}
		checkCancel()
		if data, err := json.Marshal(result); err == nil {
			_ = os.WriteFile(filepath.Join(runDir, "provider-probe.json"), data, 0o600)
		}
	}
	client.Close() // The launcher's probe is not a long-lived event subscriber.
	defer func() {
		stopCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		connection, err := daemon.Dial(stopCtx, ready.Address, token)
		if err == nil {
			_ = connection.Send(stopCtx, map[string]any{"type": "shutdown"})
			connection.Close()
		}
		_, _ = backend.Exited(5000)
	}()
	appArgs := append(append([]string(nil), plan.AppArgs...), "--connect")
	frontend, err := ownedprocess.StartWithEnvironment(binaries.app, appArgs, appDir, plan.WorkDir, withEnvironment(plan.Environment, map[string]string{"FINTWIND_MYGO_DAEMON_ADDRESS": ready.Address, "FINTWIND_MYGO_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	defer frontend.Close()
	fmt.Println("[OK] MyGo started. Close the window to stop this launcher and its daemon.")
	tick := time.NewTicker(250 * time.Millisecond)
	defer tick.Stop()
	for {
		closed, err := frontend.Exited(0)
		if err != nil {
			return err
		}
		if closed {
			code, err := frontend.ExitCode()
			if err != nil {
				return err
			}
			if code != 0 {
				return fmt.Errorf("MyGo exited with code %d; see %s", code, frontend.StderrPath)
			}
			return nil
		}
		dead, err := backend.Exited(0)
		if err != nil {
			return err
		}
		if dead {
			return fmt.Errorf("launcher-owned daemon exited; see %s", backend.StderrPath)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-tick.C:
		}
	}
}

func waitReady(ctx context.Context, process *ownedprocess.Owned, timeout time.Duration) (daemonReady, error) {
	deadline, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	tick := time.NewTicker(50 * time.Millisecond)
	defer tick.Stop()
	for {
		data, _ := os.ReadFile(process.StdoutPath)
		line, _, found := strings.Cut(string(data), "\n")
		var ready daemonReady
		if found && json.Unmarshal([]byte(line), &ready) == nil && ready.Address != "" {
			return ready, nil
		}
		exited, err := process.Exited(0)
		if err != nil {
			return ready, err
		}
		if exited {
			log, _ := os.ReadFile(process.StderrPath)
			if len(log) > 8192 {
				log = log[len(log)-8192:]
			}
			return ready, fmt.Errorf("daemon refused startup: %s", strings.TrimSpace(string(log)))
		}
		select {
		case <-deadline.Done():
			return ready, fmt.Errorf("daemon startup deadline reached; see %s", process.StderrPath)
		case <-tick.C:
		}
	}
}

func withEnvironment(base []string, overrides map[string]string) []string {
	var result []string
	for _, entry := range base {
		key, _, _ := strings.Cut(entry, "=")
		if _, replace := overrides[strings.ToUpper(key)]; !replace {
			result = append(result, entry)
		}
	}
	for key, value := range overrides {
		result = append(result, key+"="+value)
	}
	sort.Slice(result, func(i, j int) bool { return strings.ToUpper(result[i]) < strings.ToUpper(result[j]) })
	return result
}
