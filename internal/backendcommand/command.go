// Package backendcommand runs explicit daemon CLI operations in owned Jobs.
package backendcommand

import (
	"context"
	"errors"
	"io"
	"os"
	"time"

	"fintwind-go/internal/ownedprocess"
)

// Run never inherits credentials. A caller may add explicit environment
// entries after validating that operation's source and isolation contract.
func Run(ctx context.Context, binary string, args []string, root, cwd string, extra map[string]string, timeout time.Duration) ([]byte, error) {
	deadline, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	logs, err := ownedprocess.NewRoundDir()
	if err != nil {
		return nil, err
	}
	process, err := ownedprocess.StartWithEnvironment(binary, args, logs, cwd, ownedprocess.BuildEnv(root, extra))
	if err != nil {
		return nil, err
	}
	defer process.Close()
	for {
		done, err := process.Exited(50)
		if err != nil {
			return nil, err
		}
		for _, path := range []string{process.StdoutPath, process.StderrPath} {
			if info, err := os.Stat(path); err == nil && info.Size() > 2<<20 {
				return nil, errors.New("daemon command output exceeds budget")
			}
		}
		if done {
			code, err := process.ExitCode()
			if err != nil {
				return nil, err
			}
			if code != 0 {
				return nil, errors.New("daemon command failed")
			}
			file, err := os.Open(process.StdoutPath)
			if err != nil {
				return nil, err
			}
			defer file.Close()
			return io.ReadAll(io.LimitReader(file, 2<<20))
		}
		if err := deadline.Err(); err != nil {
			return nil, err
		}
	}
}
