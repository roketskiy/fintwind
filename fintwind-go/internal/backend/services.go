package backend

import (
	"context"
	"errors"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/backendservices"
	"fintwind-go/internal/backendstore"
)

type servicePool struct {
	mu           sync.Mutex
	root         string
	settingsPath string
	daily        bool
	processes    map[string]*backendengine.Process
	closed       bool
}

func (p *servicePool) acquire(ctx context.Context, c backendstore.Object) (backendservices.API, error) {
	binary := c.String("binary")
	if binary == "" {
		binary = c.String("binaryOverride")
	}
	if binary == "" {
		settings, err := readObject(p.settingsPath)
		if err != nil {
			return nil, err
		}
		binary = settings.String("opencode_binary_path")
		if binary == "" {
			binary = "opencode"
		}
	}
	found, err := exec.LookPath(binary)
	if err != nil {
		return nil, errors.New("OpenCode binary unavailable")
	}
	binary, err = filepath.Abs(found)
	if err != nil {
		return nil, err
	}
	directory := c.String("directory")
	if directory == "" {
		if p.daily {
			directory = filepath.Join(p.root, "projects")
		} else {
			directory = filepath.Join(p.root, "workspace")
		}
	}
	directory, err = filepath.EvalSymlinks(directory)
	if err != nil {
		return nil, err
	}
	if !p.daily {
		workspace, err := filepath.EvalSymlinks(filepath.Join(p.root, "workspace"))
		if err != nil {
			return nil, err
		}
		rel, err := filepath.Rel(workspace, directory)
		if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
			return nil, errors.New("service directory outside isolated workspace")
		}
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closed {
		return nil, errors.New("service pool closed")
	}
	key := binary + "\x00" + directory
	if existing := p.processes[key]; existing != nil {
		return existing.Client, nil
	}
	if !p.daily && len(p.processes) >= 4 {
		return nil, errors.New("service process limit reached")
	}
	process, err := backendengine.Start(ctx, binary, p.root, directory, p.settingsPath, p.daily)
	if err != nil {
		return nil, err
	}
	p.processes[key] = process
	return process.Client, nil
}

func (p *servicePool) close() {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.closed = true
	for _, process := range p.processes {
		_ = process.Close()
	}
	p.processes = nil
}

// reset ends only pool-owned engines after the caller has excluded every
// service RPC and runtime start. The pool remains usable after a failed update.
func (p *servicePool) reset() error {
	p.mu.Lock()
	processes := p.processes
	p.processes = map[string]*backendengine.Process{}
	p.mu.Unlock()
	var result error
	for _, process := range processes {
		result = errors.Join(result, process.Close())
	}
	return result
}
