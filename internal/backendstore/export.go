package backendstore

import (
	"bytes"
	"context"
	"database/sql"
	"database/sql/driver"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"

	"fintwind-go/internal/backendprivate"
	"github.com/google/uuid"
	"modernc.org/sqlite"
)

type ExportReport struct {
	Projects          int  `json:"projects"`
	Sessions          int  `json:"sessions"`
	Sanitized         bool `json:"sanitized"`
	SourceReadOnly    bool `json:"sourceReadOnly"`
	CredentialsCopied bool `json:"credentialsCopied"`
}

func ExportSanitized(ctx context.Context, sourcePath, targetDirectory string) (ExportReport, error) {
	report := ExportReport{SourceReadOnly: true}
	if !filepath.IsAbs(sourcePath) || !filepath.IsAbs(targetDirectory) {
		return report, errors.New("absolute export paths required")
	}
	if _, err := os.Stat(targetDirectory); !errors.Is(err, os.ErrNotExist) {
		return report, errors.New("export target must not exist")
	}
	source, err := sql.Open("sqlite", "file:"+filepath.ToSlash(sourcePath)+"?mode=ro")
	if err != nil {
		return report, err
	}
	defer source.Close()
	source.SetMaxOpenConns(1)
	var pages, size int64
	if err := source.QueryRowContext(ctx, `PRAGMA page_count`).Scan(&pages); err != nil {
		return report, err
	}
	if err := source.QueryRowContext(ctx, `PRAGMA page_size`).Scan(&size); err != nil {
		return report, err
	}
	if pages < 0 || size <= 0 || pages > (512<<20)/size {
		return report, errors.New("source exceeds in-memory export budget")
	}
	connection, err := source.Conn(ctx)
	if err != nil {
		return report, err
	}
	defer connection.Close()
	memoryURI := "file:export-" + uuid.NewString() + "?mode=memory&cache=shared"
	var retained driver.Conn
	err = connection.Raw(func(raw any) error {
		capable, ok := raw.(interface {
			NewBackup(string) (*sqlite.Backup, error)
		})
		if !ok {
			return errors.New("SQLite backup capability missing")
		}
		backup, err := capable.NewBackup(memoryURI)
		if err != nil {
			return err
		}
		var copied int64
		for {
			if err := ctx.Err(); err != nil {
				_ = backup.Finish()
				return err
			}
			copied += 128
			if copied > (512<<20)/size+128 {
				_ = backup.Finish()
				return errors.New("backup work budget exceeded")
			}
			more, err := backup.Step(128)
			if err != nil {
				_ = backup.Finish()
				return err
			}
			if !more {
				break
			}
		}
		retained, err = backup.Commit()
		return err
	})
	if err != nil {
		return report, err
	}
	defer retained.Close()
	memory, err := sql.Open("sqlite", memoryURI)
	if err != nil {
		return report, err
	}
	defer memory.Close()
	memory.SetMaxOpenConns(1)
	if err := migrate(ctx, memory); err != nil {
		return report, err
	}
	snapshot := &Store{read: memory}
	catalog, err := snapshot.Load(ctx, "")
	if err != nil {
		return report, err
	}
	var projects, sessions []Object
	if err := json.Unmarshal(catalog["projects"], &projects); err != nil {
		return report, err
	}
	if err := json.Unmarshal(catalog["sessions"], &sessions); err != nil {
		return report, err
	}
	if len(projects) > 10000 || len(sessions) > 100000 {
		return report, errors.New("export item budget exceeded")
	}
	if err := os.Mkdir(targetDirectory, 0o700); err != nil {
		return report, err
	}
	if err := backendprivate.Directory(targetDirectory); err != nil {
		return report, errors.New("cannot protect sanitized output")
	}
	target, err := Open(ctx, targetDirectory)
	if err != nil {
		return report, err
	}
	defer target.Close()
	salt := uuid.New()
	for _, project := range projects {
		project.Set("id", exportID(salt, project.String("id")))
		project.Set("name", "sanitized project")
		project.Set("path", filepath.Join(targetDirectory, "workspace", project.String("id")))
		if err := os.MkdirAll(project.String("path"), 0o700); err != nil {
			return report, err
		}
	}
	if _, err := target.Save(ctx, Fields(map[string]any{"projects": projects, "sessions": []any{}})); err != nil {
		return report, err
	}
	for _, list := range sessions {
		payload, err := snapshot.Hydrate(ctx, list.String("id"))
		if err != nil {
			return report, err
		}
		var session Object
		if err := json.Unmarshal(payload["session"], &session); err != nil {
			return report, err
		}
		encoded, err := json.Marshal(session)
		if err != nil {
			return report, err
		}
		decoder := json.NewDecoder(bytes.NewReader(encoded))
		decoder.UseNumber()
		var value any
		if err := decoder.Decode(&value); err != nil {
			return report, err
		}
		clean, err := sanitizeValue(salt, "", value)
		if err != nil {
			return report, err
		}
		encoded, err = json.Marshal(clean)
		if err != nil {
			return report, err
		}
		session, err = Decode(encoded)
		if err != nil {
			return report, err
		}
		session.Set("provider", "openCode")
		session.Set("status", "idle")
		session.Set("workspace", map[string]string{"kind": "local"})
		for _, key := range []string{"provider_cursor", "native_session_id", "runtime_event_cursor", "compaction", "goal"} {
			session.Set(key, nil)
		}
		for _, key := range []string{"queued_messages", "background_work"} {
			session.Set(key, []any{})
		}
		if _, err := target.Save(ctx, Fields(map[string]any{"projects": []any{}, "sessions": []any{session}})); err != nil {
			return report, err
		}
	}
	var integrity string
	if err := target.read.QueryRowContext(ctx, `PRAGMA integrity_check`).Scan(&integrity); err != nil {
		return report, err
	}
	if integrity != "ok" {
		return report, errors.New("export integrity check failed")
	}
	report.Projects = len(projects)
	report.Sessions = len(sessions)
	report.Sanitized = true
	encoded, err := json.MarshalIndent(report, "", "  ")
	if err != nil {
		return report, err
	}
	if err := os.WriteFile(filepath.Join(targetDirectory, "sanitized-export.json"), encoded, 0o600); err != nil {
		return report, err
	}
	return report, nil
}

func exportID(salt uuid.UUID, value string) string { return uuid.NewSHA1(salt, []byte(value)).String() }

var exportKeys = func() map[string]bool {
	keys := strings.Fields(`id project_id session_id turn_id title auto_title provider model runtime_mode interaction_mode status reasoning_effort service_tier context_window agent_preset provider_cursor created_at updated_at last_reply_at imported archived native_session_id messages transcript_blocks turns compaction queued_messages context_usage runtime_event_cursor mermaid_prompt_version workspace kind path branch baseBranch role content display_content attachments streaming activities data phase part text delta name type position stats usage references label block input_id receipt prompt files state source tokens window total_tokens cache_read prompt_tokens first_token_ms first_token_turns reasoning composition version system tools memory toolCalls toolResults opaqueReasoning opaqueMemory incomplete measured output input cached total cost duration_ms started_at completed_at success summary checkpoint turn_count git_ref additions deletions ordinal message_id items completion`)
	result := map[string]bool{}
	for _, key := range keys {
		result[key] = true
	}
	return result
}()

func sanitizeValue(salt uuid.UUID, key string, value any) (any, error) {
	switch value := value.(type) {
	case map[string]any:
		result := map[string]any{}
		for field, child := range value {
			if field == "attachments" {
				result[field] = []any{}
				continue
			}
			clean, err := sanitizeValue(salt, field, child)
			if err != nil {
				return nil, err
			}
			name := field
			if !exportKeys[field] {
				sum := digest([]byte(field))
				name = "redacted_" + hex.EncodeToString(sum[:6])
			}
			result[name] = clean
		}
		return result, nil
	case []any:
		result := make([]any, len(value))
		for i, child := range value {
			clean, err := sanitizeValue(salt, key, child)
			if err != nil {
				return nil, err
			}
			result[i] = clean
		}
		return result, nil
	case string:
		if _, err := uuid.Parse(value); err == nil {
			return exportID(salt, value), nil
		}
		if key == "role" && (value == "user" || value == "assistant" || value == "system") {
			return value, nil
		}
		if key == "status" && (value == "completed" || value == "failed" || value == "interrupted" || value == "ready" || value == "unavailable" || value == "error") {
			return value, nil
		}
		if key == "status" && value == "running" {
			return "interrupted", nil
		}
		if key == "kind" && (value == "text" || value == "reasoning" || value == "activity" || value == "activities" || value == "command" || value == "fileChange" || value == "fileRead" || value == "fileSearch" || value == "fileList" || value == "search" || value == "plan" || value == "tool" || value == "local") {
			return value, nil
		}
		if key == "runtime_mode" {
			return "ask", nil
		}
		if key == "interaction_mode" {
			return "build", nil
		}
		if len(value) > 16<<20 {
			return nil, errors.New("export string exceeds budget")
		}
		return strings.Repeat("x", len(value)), nil
	default:
		return value, nil
	}
}
