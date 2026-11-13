package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/daemon"
	"fintwind-go/internal/ownedprocess"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

func checkNegativeStartup(ctx context.Context, binary string) error {
	missing, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	missingLogs := filepath.Join(missing, "logs")
	if err := os.MkdirAll(missingLogs, 0o700); err != nil {
		return err
	}
	child, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", filepath.Join(missing, "data"), "--bind", "127.0.0.1:0"}, missingLogs, missing, ownedprocess.BuildEnv(missing, nil))
	if err != nil {
		return err
	}
	defer child.Close()
	if done, err := child.Exited(5000); err != nil || !done {
		return errors.New("daemon without token stayed alive")
	}
	code, err := child.ExitCode()
	if err != nil || code == 0 {
		return errors.New("daemon without token exited cleanly")
	}
	raw, _ := os.ReadFile(child.StdoutPath)
	var ready struct {
		Address string `json:"address"`
	}
	if json.Unmarshal(raw, &ready) == nil && ready.Address != "" {
		return errors.New("daemon without token reported ready")
	}
	loopback, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	loopbackLogs := filepath.Join(loopback, "logs")
	if err := os.MkdirAll(loopbackLogs, 0o700); err != nil {
		return err
	}
	token := uuid.NewString()
	nonLoopback, err := ownedprocess.StartWithEnvironment(binary, []string{"--data-dir", filepath.Join(loopback, "data"), "--bind", "0.0.0.0:0"}, loopbackLogs, loopback, ownedprocess.BuildEnv(loopback, map[string]string{"FINTWIND_DAEMON_TOKEN": token}))
	if err != nil {
		return err
	}
	defer nonLoopback.Close()
	if done, err := nonLoopback.Exited(5000); err != nil || !done {
		return errors.New("daemon with non-loopback bind stayed alive")
	}
	if code, err := nonLoopback.ExitCode(); err != nil || code == 0 {
		return errors.New("daemon with non-loopback bind exited cleanly")
	}
	return nil
}

func checkProtocolMismatch(ctx context.Context, address, token string) error {
	dialer := websocket.Dialer{HandshakeTimeout: 10 * time.Second}
	conn, _, err := dialer.DialContext(ctx, "ws://"+address+"/v1", nil)
	if err != nil {
		return err
	}
	defer conn.Close()
	_ = conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	if err := conn.WriteJSON(map[string]any{"type": "hello", "protocolVersion": daemon.ProtocolVersion + 1, "token": token, "clientId": uuid.NewString(), "resumeFrom": []any{}}); err != nil {
		return err
	}
	var reply struct {
		Type string `json:"type"`
	}
	if err := conn.ReadJSON(&reply); err != nil {
		return err
	}
	if reply.Type != "rejected" {
		return errors.New("protocol mismatch was not rejected")
	}
	return nil
}
