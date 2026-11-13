package backendmcpauth

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync/atomic"
	"time"

	"fintwind-go/internal/ownedprocess"
)

func (s *Service) run(ctx context.Context, binary, directory, name string, extra map[string]string) error {
	stdout, outWrite, err := os.Pipe()
	if err != nil {
		return errors.New("cannot create MCP authorization stream")
	}
	defer stdout.Close()
	defer outWrite.Close()
	stderr, errWrite, err := os.Pipe()
	if err != nil {
		return errors.New("cannot create MCP authorization stream")
	}
	defer stderr.Close()
	defer errWrite.Close()
	environment := map[string]string{
		"OPENCODE_CONFIG_DIR": filepath.Join(s.root, "home", "opencode"), "OPENCODE_DB": filepath.Join(s.root, "data", "opencode.db"), "OPENCODE_TEST_HOME": filepath.Join(s.root, "home"), "OPENCODE_DISABLE_PROJECT_CONFIG": "true",
	}
	// Only validated proxy configuration crosses this boundary, never keys or
	// arbitrary caller environment. Profile/config paths cannot be overridden.
	for _, key := range []string{"HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY"} {
		if value := extra[key]; value != "" {
			environment[key] = value
		}
	}
	env := ownedprocess.BuildEnv(s.root, environment)
	if s.daily {
		env = ownedprocess.InheritEnv(extra)
	}
	child, err := ownedprocess.StartWithOutput(binary, []string{"mcp", "auth", name}, s.root, directory, env, outWrite, errWrite)
	_ = outWrite.Close()
	_ = errWrite.Close()
	if err != nil {
		return errors.New("could not start isolated MCP authorization")
	}
	defer child.Close()
	events := make(chan outputEvent, 32)
	var total atomic.Int64
	go readOutput(stdout, events, &total)
	go readOutput(stderr, events, &total)
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	var failure error
	done := 0
	exited := false
	opened := false
	var code uint32
	var drain <-chan time.Time
	var drainTimer *time.Timer
	defer func() {
		if drainTimer != nil {
			drainTimer.Stop()
		}
	}()
	finish := func() {
		if exited {
			return
		}
		exited = true
		code, err = child.ExitCode()
		if err != nil && failure == nil {
			failure = errors.New("cannot read MCP authorization exit status")
		}
		if err := child.Close(); err != nil && failure == nil {
			failure = errors.New("cannot close MCP authorization process")
		}
		drainTimer = time.NewTimer(5 * time.Second)
		drain = drainTimer.C
	}
	cancelled := ctx.Done()
	for done < 2 || !exited {
		select {
		case event := <-events:
			if event.done {
				done++
				continue
			}
			if event.err != nil && failure == nil {
				failure = event.err
				finish()
				continue
			}
			if !opened && failure == nil && ctx.Err() == nil {
				if target := authorizationURL(event.line); target != "" {
					opened = true
					if err := s.opener(target); err != nil {
						failure = errors.New("could not open MCP authorization page in the system browser")
						finish()
					}
				}
			}
		case <-cancelled:
			cancelled = nil
			if failure == nil {
				failure = errors.New("MCP authorization cancelled or timed out")
			}
			finish()
		case <-ticker.C:
			if !exited {
				ended, err := child.Exited(0)
				if err != nil {
					failure = errors.New("cannot monitor MCP authorization process")
					finish()
				} else if ended {
					finish()
				}
			}
		case <-drain:
			drain = nil
			_ = stdout.Close()
			_ = stderr.Close()
			if failure == nil {
				failure = errors.New("MCP authorization output did not close")
			}
		}
	}
	if failure != nil {
		return failure
	}
	if code != 0 {
		return errors.New("MCP authorization did not finish successfully")
	}
	return nil
}
