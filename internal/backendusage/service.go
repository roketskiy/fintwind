package backendusage

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"sync"
	"time"
)

type Factory func(context.Context) (API, func(), error)
type slot struct {
	snapshot Stats
	checked  time.Time
	running  bool
}
type Service struct {
	mu      sync.Mutex
	ctx     context.Context
	cancel  context.CancelFunc
	workers sync.WaitGroup
	cache   *index
	slots   map[string]*slot
	closed  bool
}

func New(ctx context.Context, path string) (*Service, error) {
	cache, err := openIndex(ctx, path)
	if err != nil {
		return nil, err
	}
	child, cancel := context.WithCancel(ctx)
	return &Service{ctx: child, cancel: cancel, cache: cache, slots: map[string]*slot{}}, nil
}

func cloneStats(stats Stats) (Stats, error) {
	raw, err := json.Marshal(stats)
	if err != nil {
		return Stats{}, err
	}
	var clone Stats
	err = json.Unmarshal(raw, &clone)
	if clone.Entries == nil {
		clone.Entries = []Entry{}
	}
	return clone, err
}

// Snapshot never acquires an engine on the caller's path. Its factory runs
// only inside the daemon-owned worker, and cleanup releases the host gate.
func (s *Service) Snapshot(ctx context.Context, source string, force bool, factory Factory) (Stats, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return Stats{}, errors.New("usage index closed")
	}
	current := s.slots[source]
	if current == nil {
		// Retain all durable sources; only in-memory idle handles are evicted.
		if len(s.slots) >= 16 {
			oldest := ""
			var checked time.Time
			for key, value := range s.slots {
				if !value.running && (oldest == "" || value.checked.Before(checked)) {
					oldest = key
					checked = value.checked
				}
			}
			if oldest == "" {
				return Stats{}, errors.New("usage scan capacity reached")
			}
			delete(s.slots, oldest)
		}
		snapshot, err := s.cache.snapshot(ctx, source)
		if err != nil {
			return Stats{}, err
		}
		current = &slot{snapshot: snapshot}
		s.slots[source] = current
	}
	if !current.running && (force || current.checked.IsZero() || time.Since(current.checked) >= 30*time.Second) {
		current.running = true
		current.snapshot.Refreshing = true
		current.snapshot.RefreshError = nil
		s.workers.Add(1)
		go s.refresh(source, current, factory)
	}
	return cloneStats(current.snapshot)
}

func (s *Service) refresh(source string, current *slot, factory Factory) {
	defer s.workers.Done()
	// Each request and session walk has its own budget. A large store is not
	// cut off by an unrelated wall-clock limit; Close cancels every worker.
	ctx, cancel := context.WithCancel(s.ctx)
	defer cancel()
	api, release, err := factory(ctx)
	var stats Stats
	if err == nil {
		if release != nil {
			defer release()
		}
		stats, err = scan(ctx, s.cache, source, api, func(progress Stats) { s.mu.Lock(); current.snapshot = progress; s.mu.Unlock() })
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	current.running = false
	current.checked = time.Now()
	current.snapshot.Refreshing = false
	if err != nil {
		message := refreshError(err)
		current.snapshot.RefreshError = &message
		return
	}
	current.snapshot = stats
}

func refreshError(err error) string {
	if errors.Is(err, context.Canceled) {
		return "usage refresh cancelled; previous verified usage retained"
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return "usage request timed out; previous verified usage retained"
	}
	var network net.Error
	if errors.As(err, &network) {
		return "usage provider connection failed; previous verified usage retained"
	}
	// Only daemon-authored fixed categories are safe for UI diagnostics. Raw
	// transport/database errors can contain proxy credentials or host paths.
	switch err.Error() {
	case "invalid usage session facts", "invalid usage session listing", "usage listing byte budget exceeded", "duplicate usage session identity", "usage session listing incomplete", "usage session listing exceeded page budget", "invalid cached usage split", "OpenCode management is in progress", "service process limit reached", "service pool closed", "OpenCode binary unavailable":
		return err.Error() + "; previous verified usage retained"
	default:
		return "usage refresh failed; previous verified usage retained"
	}
}

func (s *Service) Close() error {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return nil
	}
	s.closed = true
	s.cancel()
	s.mu.Unlock()
	s.workers.Wait()
	return s.cache.db.Close()
}
