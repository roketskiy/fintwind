package backendworkspace

import (
	"context"
	"errors"
	"sync"
	"time"

	"github.com/google/uuid"
)

type query struct {
	cancel context.CancelFunc
	until  time.Time
}

// Queries belongs to one daemon, not the machine or a process-name registry.
type Queries struct {
	mu    sync.Mutex
	items map[string]query
}

func (q *Queries) prune() {
	for id, item := range q.items {
		if item.cancel == nil && time.Now().After(item.until) {
			delete(q.items, id)
		}
	}
}
func (q *Queries) Cancel(id string) error {
	if uuid.Validate(id) != nil {
		return errors.New("valid Git query ID required")
	}
	q.mu.Lock()
	defer q.mu.Unlock()
	if q.items == nil {
		q.items = map[string]query{}
	}
	q.prune()
	if item, ok := q.items[id]; ok && item.cancel != nil {
		item.cancel()
		return nil
	}
	if len(q.items) >= 4096 {
		return errors.New("Git cancellation registry full")
	}
	q.items[id] = query{until: time.Now().Add(time.Minute)}
	return nil
}
func (q *Queries) Begin(ctx context.Context, id string) (context.Context, func(), error) {
	if id == "" {
		return ctx, func() {}, nil
	}
	if uuid.Validate(id) != nil {
		return nil, nil, errors.New("valid Git query ID required")
	}
	q.mu.Lock()
	defer q.mu.Unlock()
	if q.items == nil {
		q.items = map[string]query{}
	}
	q.prune()
	if item, ok := q.items[id]; ok {
		if item.cancel != nil {
			return nil, nil, errors.New("Git query identity already active")
		}
		return nil, nil, errors.New("Git query cancelled")
	}
	if len(q.items) >= 4096 {
		return nil, nil, errors.New("Git query registry full")
	}
	child, cancel := context.WithCancel(ctx)
	q.items[id] = query{cancel: cancel}
	return child, func() { cancel(); q.mu.Lock(); delete(q.items, id); q.mu.Unlock() }, nil
}
