package backendusage

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/url"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

type index struct{ db *sql.DB }

func openIndex(ctx context.Context, path string) (*index, error) {
	if !filepath.IsAbs(path) {
		return nil, errors.New("usage index requires absolute path")
	}
	query := url.Values{}
	query.Add("_pragma", "busy_timeout(5000)")
	query.Add("_pragma", "journal_mode(WAL)")
	query.Set("_txlock", "immediate")
	db, err := sql.Open("sqlite", "file:"+filepath.ToSlash(path)+"?"+query.Encode())
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	_, err = db.ExecContext(ctx, `CREATE TABLE IF NOT EXISTS usage_splits_v1(source TEXT NOT NULL, session TEXT NOT NULL, fingerprint TEXT NOT NULL, checked INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(source,session));
CREATE TABLE IF NOT EXISTS usage_snapshots_v1(source TEXT PRIMARY KEY, payload TEXT NOT NULL);`)
	if err != nil {
		db.Close()
		return nil, err
	}
	return &index{db: db}, nil
}

func (i *index) snapshot(ctx context.Context, source string) (Stats, error) {
	stats := Stats{Entries: []Entry{}}
	var raw string
	err := i.db.QueryRowContext(ctx, `SELECT payload FROM usage_snapshots_v1 WHERE source=?`, source).Scan(&raw)
	if errors.Is(err, sql.ErrNoRows) {
		return stats, nil
	}
	if err != nil {
		return stats, err
	}
	err = json.Unmarshal([]byte(raw), &stats)
	if stats.Entries == nil {
		stats.Entries = []Entry{}
	}
	stats.Refreshing = false
	return stats, err
}

func (i *index) saveSnapshot(ctx context.Context, source string, stats Stats) error {
	raw, err := json.Marshal(stats)
	if err != nil {
		return err
	}
	_, err = i.db.ExecContext(ctx, `INSERT INTO usage_snapshots_v1(source,payload) VALUES(?,?) ON CONFLICT(source) DO UPDATE SET payload=excluded.payload`, source, string(raw))
	return err
}

func (i *index) split(ctx context.Context, source, id, fingerprint string, recent bool, now time.Time) (json.RawMessage, bool, error) {
	var raw, stored string
	var checked int64
	err := i.db.QueryRowContext(ctx, `SELECT fingerprint,checked,payload FROM usage_splits_v1 WHERE source=? AND session=?`, source, id).Scan(&stored, &checked, &raw)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, false, nil
	}
	if err != nil {
		return nil, false, err
	}
	if !json.Valid([]byte(raw)) {
		return nil, false, errors.New("invalid cached usage split")
	}
	fresh := stored == fingerprint && (!recent || now.Unix() >= checked && now.Unix()-checked < 300)
	return json.RawMessage(raw), fresh, nil
}

func (i *index) saveSplit(ctx context.Context, source, id, fingerprint string, raw json.RawMessage, now time.Time) error {
	if !json.Valid(raw) {
		return errors.New("invalid usage split")
	}
	_, err := i.db.ExecContext(ctx, `INSERT INTO usage_splits_v1(source,session,fingerprint,checked,payload) VALUES(?,?,?,?,?) ON CONFLICT(source,session) DO UPDATE SET fingerprint=excluded.fingerprint,checked=excluded.checked,payload=excluded.payload`, source, id, fingerprint, now.Unix(), string(raw))
	return err
}

func (i *index) retain(ctx context.Context, source string, seen map[string]bool) error {
	tx, err := i.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	rows, err := tx.QueryContext(ctx, `SELECT session FROM usage_splits_v1 WHERE source=?`, source)
	if err != nil {
		return err
	}
	remove := []string{}
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			rows.Close()
			return err
		}
		if !seen[id] {
			remove = append(remove, id)
		}
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return err
	}
	for _, id := range remove {
		if _, err := tx.ExecContext(ctx, `DELETE FROM usage_splits_v1 WHERE source=? AND session=?`, source, id); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// SourceKey identifies the provider store, not a project directory or a
// transient server port. Values are supplied by the isolated engine host.
func SourceKey(binary string, environment map[string]string) (string, error) {
	canonical, err := filepath.EvalSymlinks(binary)
	if err != nil {
		return "", err
	}
	canonical, err = filepath.Abs(canonical)
	if err != nil {
		return "", err
	}
	// std::fs::canonicalize uses verbatim Windows paths in Rust's source key.
	if runtime.GOOS == "windows" && !strings.HasPrefix(canonical, `\\?\`) {
		if strings.HasPrefix(canonical, `\\`) {
			canonical = `\\?\UNC\` + strings.TrimPrefix(canonical, `\\`)
		} else {
			canonical = `\\?\` + canonical
		}
	}
	hash := sha256.New()
	hash.Write([]byte(canonical))
	for _, key := range []string{"XDG_DATA_HOME", "XDG_CONFIG_HOME", "HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"} {
		hash.Write([]byte{0})
		hash.Write([]byte(key))
		hash.Write([]byte(environment[key]))
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}
