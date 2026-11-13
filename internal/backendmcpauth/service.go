// Package backendmcpauth owns isolated OpenCode CLI OAuth flows. Authorization
// output travels through bounded pipes and is never written to daemon logs.
package backendmcpauth

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
	"unicode"

	"fintwind-go/internal/backendprivate"
)

type OpenURL func(string) error
type flow struct {
	cancel context.CancelFunc
	done   chan struct{}
}
type Service struct {
	mu      sync.Mutex
	ctx     context.Context
	cancel  context.CancelFunc
	root    string
	daily   bool
	opener  OpenURL
	pending map[string]*flow
	workers sync.WaitGroup
	closed  bool
}

func New(ctx context.Context, root string, daily bool, opener OpenURL) *Service {
	child, cancel := context.WithCancel(ctx)
	if opener == nil {
		opener = openSystemURL
	}
	return &Service{ctx: child, cancel: cancel, root: root, daily: daily, opener: opener, pending: map[string]*flow{}}
}

func ValidateName(name string) error {
	if strings.TrimSpace(name) == "" || len(name) > 256 || strings.HasPrefix(name, "-") {
		return errors.New("MCP server name is empty or invalid")
	}
	for _, r := range name {
		if unicode.IsControl(r) {
			return errors.New("MCP server name contains control characters")
		}
	}
	return nil
}

func (s *Service) Authenticate(ctx context.Context, binary, directory, name string, extra map[string]string) error {
	if err := ValidateName(name); err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, 360*time.Second)
	defer cancel()
	stop := context.AfterFunc(s.ctx, cancel)
	defer stop()
	entry := &flow{cancel: cancel, done: make(chan struct{})}
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return errors.New("MCP authorization service closed")
	}
	if s.pending[name] != nil {
		s.mu.Unlock()
		return errors.New("MCP authorization is already in progress for this server")
	}
	if len(s.pending) >= 16 {
		s.mu.Unlock()
		return errors.New("MCP authorization capacity reached")
	}
	s.pending[name] = entry
	s.workers.Add(1)
	s.mu.Unlock()
	defer func() {
		s.mu.Lock()
		if s.pending[name] == entry {
			delete(s.pending, name)
		}
		close(entry.done)
		s.mu.Unlock()
		s.workers.Done()
	}()
	if err := ctx.Err(); err != nil {
		return errors.New("MCP authorization cancelled")
	}
	if !filepath.IsAbs(binary) || !filepath.IsAbs(directory) || !filepath.IsAbs(s.root) {
		return errors.New("absolute MCP authorization paths required")
	}
	root, err := filepath.EvalSymlinks(s.root)
	if err != nil {
		return errors.New("isolated MCP profile unavailable")
	}
	cwd, err := filepath.EvalSymlinks(directory)
	if err != nil {
		return errors.New("MCP authorization directory unavailable")
	}
	if s.daily {
		return s.run(ctx, binary, cwd, name, extra)
	}
	rel, err := filepath.Rel(filepath.Join(root, "workspace"), cwd)
	if err != nil || !filepath.IsLocal(rel) {
		return errors.New("MCP authorization directory outside isolated workspace")
	}
	info, err := os.Stat(cwd)
	if err != nil || !info.IsDir() {
		return errors.New("MCP authorization directory unavailable")
	}
	if err := backendprivate.Directory(root); err != nil {
		return errors.New("cannot protect isolated MCP profile")
	}
	profile, err := os.OpenRoot(root)
	if err != nil {
		return errors.New("isolated MCP profile unavailable")
	}
	defer profile.Close()
	for _, sub := range []string{"tmp", "data", "home/opencode"} {
		if err := profile.MkdirAll(sub, 0o700); err != nil {
			return errors.New("cannot prepare isolated MCP profile")
		}
		canonical, e := filepath.EvalSymlinks(filepath.Join(root, sub))
		if e != nil {
			return errors.New("isolated MCP profile unavailable")
		}
		relative, e := filepath.Rel(root, canonical)
		if e != nil || !filepath.IsLocal(relative) {
			return errors.New("MCP profile path outside isolated directory")
		}
	}
	return s.run(ctx, binary, cwd, name, extra)
}

// Cancel affects only the currently registered flow. Its registration is kept
// until its Job and both output readers are gone, preventing callback overlap.
func (s *Service) Cancel(name string) (bool, error) {
	if err := ValidateName(name); err != nil {
		return false, err
	}
	s.mu.Lock()
	entry := s.pending[name]
	s.mu.Unlock()
	if entry == nil {
		return false, nil
	}
	entry.cancel()
	return true, nil
}

func (s *Service) Close() {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return
	}
	s.closed = true
	s.cancel()
	for _, entry := range s.pending {
		entry.cancel()
	}
	s.mu.Unlock()
	s.workers.Wait()
}
