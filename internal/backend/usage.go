package backend

import (
	"context"
	"errors"
	"os"
	"path/filepath"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/backendusage"
)

func (s *Server) usageStats(ctx context.Context, c backendstore.Object) (backendstore.Object, error) {
	binary, err := s.providerBinary(c.String("binary"))
	if err != nil {
		return nil, err
	}
	home := filepath.Join(filepath.Dir(s.Directory), "home")
	environment := map[string]string{}
	for _, key := range []string{"XDG_DATA_HOME", "XDG_CONFIG_HOME", "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"} {
		environment[key] = home
		if s.daily {
			environment[key] = os.Getenv(key)
		}
	}
	source, err := backendusage.SourceKey(binary, environment)
	if err != nil {
		return nil, err
	}
	s.usageMu.Lock()
	if s.usageClosed {
		s.usageMu.Unlock()
		return nil, errors.New("usage index closed")
	}
	if s.usage == nil {
		s.usage, err = backendusage.New(s.lifetime, filepath.Join(s.Directory, "usage-index.db"))
	}
	usage := s.usage
	s.usageMu.Unlock()
	if err != nil {
		return nil, err
	}
	stats, err := usage.Snapshot(ctx, source, c.Bool("forceRefresh"), func(worker context.Context) (backendusage.API, func(), error) {
		if !s.managementMu.TryRLock() {
			return nil, nil, errors.New("OpenCode management is in progress")
		}
		// Every project shares this provider store. Use one fixed workspace
		// instead of consuming the process pool separately for each directory.
		api, err := s.servicePool.acquire(worker, backendstore.Fields(map[string]any{"binary": binary, "directory": s.WorkspaceRoot}))
		if err != nil {
			s.managementMu.RUnlock()
			return nil, nil, err
		}
		return api, s.managementMu.RUnlock, nil
	})
	if err != nil {
		return nil, err
	}
	return backendstore.Reply("usageStats", backendstore.Fields(map[string]any{"stats": stats})), nil
}
