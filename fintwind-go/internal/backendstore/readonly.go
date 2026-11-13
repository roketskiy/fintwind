package backendstore

import (
	"context"
	"database/sql"
	"net/url"
	"path/filepath"
)

// ReadOnly exposes the same catalog and hydration paths without opening a
// writable connection, applying migrations, or starting a daemon.
type ReadOnly struct {
	db    *sql.DB
	store *Store
}

func OpenReadOnly(ctx context.Context, path string) (*ReadOnly, error) {
	absolute, err := filepath.Abs(path)
	if err != nil {
		return nil, err
	}
	query := url.Values{}
	query.Set("mode", "ro")
	query.Add("_pragma", "query_only(1)")
	query.Add("_pragma", "busy_timeout(5000)")
	db, err := sql.Open("sqlite", "file:"+filepath.ToSlash(absolute)+"?"+query.Encode())
	if err != nil {
		return nil, err
	}
	if err := db.PingContext(ctx); err != nil {
		db.Close()
		return nil, err
	}
	return &ReadOnly{db: db, store: &Store{read: db}}, nil
}

func (r *ReadOnly) Close() error { return r.db.Close() }

func (r *ReadOnly) Load(ctx context.Context) (Object, error) {
	return r.store.Load(ctx, "")
}

func (r *ReadOnly) Hydrate(ctx context.Context, id string) (Object, error) {
	return r.store.Hydrate(ctx, id)
}
