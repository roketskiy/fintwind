package backendstore

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
)

func (s *Store) AssetReferences(ctx context.Context, directory string) (map[string]bool, error) {
	result := map[string]bool{}
	scan := func(data string) {
		for _, scheme := range []string{"fintwind-blob:", "fintwind-attachment:"} {
			rest := data
			for {
				index := strings.Index(rest, scheme)
				if index < 0 {
					break
				}
				rest = rest[index:]
				end := strings.IndexAny(rest, "\"\\\r\n\t ")
				if end < 0 {
					break
				}
				result[rest[:end]] = true
				rest = rest[end:]
			}
		}
	}
	rows, err := s.read.QueryContext(ctx, `SELECT data FROM session_details UNION ALL SELECT attachments FROM messages UNION ALL SELECT data FROM submissions`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	for rows.Next() {
		var raw string
		if err := rows.Scan(&raw); err != nil {
			return nil, err
		}
		scan(raw)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	raw, err := os.ReadFile(filepath.Join(directory, "composer-drafts.json"))
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	scan(string(raw))
	return result, nil
}
