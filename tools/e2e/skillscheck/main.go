package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"fintwind-go/internal/backendskills"
	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/ownedprocess"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}
func run() (err error) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	checks := map[string]bool{}
	defer func() {
		report := map[string]any{"checks": checks, "passed": err == nil}
		if err != nil {
			report["error"] = err.Error()
		}
		raw, e := json.MarshalIndent(report, "", "  ")
		if e == nil {
			e = os.WriteFile(filepath.Join(round, "skills-result.json"), raw, 0o600)
		}
		err = errors.Join(err, e)
		fmt.Println("[REPORT]", filepath.Join(round, "skills-result.json"))
	}()
	project := filepath.Join(round, "workspace", "project")
	if err := os.MkdirAll(project, 0o700); err != nil {
		return err
	}
	service := backendskills.New(round, filepath.Join(round, "home"), false)
	call := func(c map[string]any) (backendstore.Object, error) {
		response, handled, err := service.Handle(ctx, backendstore.Fields(c))
		if !handled && err == nil {
			return nil, errors.New("skills command unhandled")
		}
		return response, err
	}
	if _, err := call(map[string]any{"type": "loadSkills", "projects": []any{map[string]string{"name": "escape", "root": filepath.Dir(round)}}}); err == nil {
		return errors.New("skill project escape accepted")
	}
	checks["project-escape-refused"] = true
	shared := filepath.Join(round, "home", ".agents", "skills", "literal")
	native := filepath.Join(round, "home", ".config", "opencode", "skills", "literal")
	local := filepath.Join(project, ".opencode", "skills", "literal")
	for _, dir := range []string{shared, native, local} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return err
		}
		if err := os.WriteFile(filepath.Join(dir, "SKILL.md"), []byte("---\nname: literal\ndescription: Literal skill\nallowed-tools: Read\n---\nLiteral instructions"), 0o600); err != nil {
			return err
		}
	}
	load := func() ([]backendskills.Entry, error) {
		response, err := call(map[string]any{"type": "loadSkills", "projects": [][2]string{{"project", project}}})
		if err != nil {
			return nil, err
		}
		var catalog struct {
			Skills []backendskills.Entry `json:"skills"`
		}
		err = json.Unmarshal(response["catalog"], &catalog)
		return catalog.Skills, err
	}
	entries, err := load()
	if err != nil {
		return err
	}
	if len(entries) != 2 || len(entries[0].Installs) != 2 || entries[0].Duplicates != 1 || entries[1].Duplicates != 1 || entries[0].Body != "Literal instructions" || entries[0].AllowedTools == nil || *entries[0].AllowedTools != "Read" {
		return errors.New("skill grouping or frontmatter mismatch")
	}
	checks["cross-roots-grouped-cross-scopes-separate"] = true
	if _, err := call(map[string]any{"type": "setSkillsEnabled", "dirs": []string{filepath.Dir(shared)}, "enabled": false}); err == nil {
		return errors.New("skills root mutation accepted")
	}
	checks["skill-root-protected"] = true
	for i := 0; i < 2; i++ {
		if _, err := call(map[string]any{"type": "setSkillsEnabled", "dirs": []string{shared, native}, "enabled": false}); err != nil {
			return err
		}
	}
	service = backendskills.New(round, filepath.Join(round, "home"), false)
	entries, err = load()
	if err != nil {
		return err
	}
	if entries[0].Enabled {
		return errors.New("disabled skills enabled after reopen")
	}
	checks["disable-idempotent-and-durable"] = true
	if _, err := call(map[string]any{"type": "setSkillsEnabled", "dirs": []string{shared, native}, "enabled": true}); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(shared, "SKILL.md.disabled"), []byte("stale"), 0o600); err != nil {
		return err
	}
	if _, err := call(map[string]any{"type": "setSkillsEnabled", "dirs": []string{shared}, "enabled": true}); err != nil {
		return err
	}
	raw, err := os.ReadFile(filepath.Join(shared, "SKILL.md"))
	if err != nil || string(raw) == "stale" {
		return errors.New("enable overwrote live skill")
	}
	checks["enable-preserves-live"] = true
	if _, err := call(map[string]any{"type": "trashSkills", "dirs": []string{local}}); err != nil {
		return err
	}
	if _, err := os.Stat(local); !errors.Is(err, os.ErrNotExist) {
		return errors.New("trashed skill still present")
	}
	checks["owned-skill-system-trash"] = true
	return nil
}
