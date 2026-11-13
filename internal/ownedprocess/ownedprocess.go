// Package ownedprocess starts one child inside a kill-on-close Windows Job.
//
// The caller never sees a running process outside the Job: the child is
// created suspended, assigned to the Job, then resumed before Start returns.
// Cleanup only touches handles this process holds: the Job handle and the
// child process handle. No PID scan, no name or port matching.
package ownedprocess

import (
	"errors"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
)

// baseDir is the only parent verify rounds may create unique dirs under.
const baseDir = `C:\Users\Public\Temp\opencode`

// allowEnv is the exact parent environment passthrough. Anything else,
// including OPENCODE_* and FINTWIND_*, never crosses into the child.
var allowEnv = []string{"SystemRoot", "WINDIR", "COMSPEC", "PATH", "TEMP", "TMP"}

// Owned is one child contained by its Job. Close ends the Job.
type Owned struct {
	mu       sync.Mutex
	closed   bool
	closeErr error
	// PID is informational only. Never use it to find or kill processes.
	PID uint32
	// Dir is the unique isolated root created for this round.
	Dir string
	// StdoutPath and StderrPath hold the child streams for this round.
	StdoutPath string
	StderrPath string

	close func() error
	// exited reports whether the child handle already signaled.
	exited   func(timeoutMs uint32) (bool, error)
	exitCode func() (uint32, error)
}

// ExitCode reads only the process handle held by this owner.
func (o *Owned) ExitCode() (uint32, error) {
	if o == nil {
		return 0, errors.New("ownedprocess: no child handle")
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.closed || o.exitCode == nil {
		return 0, errors.New("ownedprocess: child handle is closed")
	}
	return o.exitCode()
}

// Close terminates the Job and releases held handles. It never touches
// processes outside this Owned value.
func (o *Owned) Close() error {
	if o == nil || o.close == nil {
		return nil
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	if !o.closed {
		o.closed = true
		o.closeErr = o.close()
	}
	return o.closeErr
}

// Exited reports whether the child process handle signaled within timeoutMs.
// It only waits on the held handle.
func (o *Owned) Exited(timeoutMs uint32) (bool, error) {
	if o == nil || o.exited == nil {
		return false, errors.New("ownedprocess: no child handle")
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.closed {
		return o.closeErr == nil, o.closeErr
	}
	return o.exited(timeoutMs)
}

// NewRoundDir creates the unique isolated root for one verify round under
// baseDir. It never reuses or deletes existing directories.
func NewRoundDir() (string, error) {
	if err := os.MkdirAll(baseDir, 0o755); err != nil {
		return "", err
	}
	dir, err := os.MkdirTemp(baseDir, "mygo-verify-*")
	if err != nil {
		return "", err
	}
	for _, sub := range []string{"tmp", "home"} {
		if err := os.MkdirAll(filepath.Join(dir, sub), 0o755); err != nil {
			return "", err
		}
	}
	return dir, nil
}

// StartWithInput supplies a bounded, immutable stdin file to the same owned
// Job launcher. The file stays beside the round's stdout/stderr as evidence.
func StartWithInput(executable string, args []string, runDir, workingDir string, environment []string, input []byte) (*Owned, error) {
	if len(input) > 64<<20 {
		return nil, errors.New("ownedprocess: stdin exceeds budget")
	}
	file, err := os.CreateTemp(runDir, "child-stdin-*")
	if err != nil {
		return nil, err
	}
	defer file.Close()
	if _, err := file.Write(input); err != nil {
		return nil, err
	}
	if _, err := file.Seek(0, 0); err != nil {
		return nil, err
	}
	return startWithEnvironmentInput(executable, args, runDir, workingDir, environment, file)
}

// BuildEnv returns the child environment: allowlisted parent keys plus an
// isolated root for profile-like variables plus explicit extra entries.
// Parent OPENCODE_* and FINTWIND_* keys are dropped. Extra keys win and are
// how the daemon token is passed.
func BuildEnv(roundDir string, extra map[string]string) []string {
	parent := map[string]string{}
	for _, kv := range os.Environ() {
		if index := strings.IndexByte(kv, '='); index > 0 {
			parent[strings.ToUpper(kv[:index])] = kv[index+1:]
		}
	}
	values := make(map[string]string)
	values["FINTWIND_MYGO_PROFILE"] = "agent"
	for _, key := range allowEnv {
		value, ok := parent[strings.ToUpper(key)]
		if !ok {
			continue
		}
		switch key {
		case "TEMP", "TMP":
			value = filepath.Join(roundDir, "tmp")
		}
		values[strings.ToUpper(key)] = value
	}
	home := filepath.Join(roundDir, "home")
	for _, key := range []string{"USERPROFILE", "HOME", "APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"} {
		value := home
		if key == "XDG_CACHE_HOME" {
			value = filepath.Join(roundDir, "tmp")
		}
		values[key] = value
	}
	for key, value := range extra {
		values[strings.ToUpper(key)] = value
	}
	keys := make([]string, 0, len(values))
	for key := range values {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	env := make([]string, 0, len(keys))
	for _, key := range keys {
		env = append(env, key+"="+values[key])
	}
	return env
}

// InheritEnv retains the user's normal configuration for the daily profile.
// Explicit values replace inherited entries case-insensitively on Windows.
func InheritEnv(extra map[string]string) []string {
	values := map[string]string{}
	for _, kv := range os.Environ() {
		if index := strings.IndexByte(kv, '='); index > 0 {
			values[strings.ToUpper(kv[:index])] = kv[index+1:]
		}
	}
	delete(values, "FINTWIND_DAEMON_TOKEN")
	for key, value := range extra {
		values[strings.ToUpper(key)] = value
	}
	keys := make([]string, 0, len(values))
	for key := range values {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	env := make([]string, 0, len(keys))
	for _, key := range keys {
		env = append(env, key+"="+values[key])
	}
	return env
}
