package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"

	"fintwind-go/internal/daemon"
)

func checkSkills(ctx context.Context, c *daemon.Client, round string) error {
	dir := filepath.Join(round, "home", ".agents", "skills", "literal")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(dir, "SKILL.md"), []byte("---\nname: literal\n---\nLiteral body"), 0o600); err != nil {
		return err
	}
	raw, err := c.Request(ctx, map[string]any{"type": "loadSkills", "projects": [][2]string{{"fixture", filepath.Join(round, "workspace")}}})
	if err != nil {
		return err
	}
	var response struct {
		Type    string `json:"type"`
		Catalog struct {
			Skills []struct {
				Name string `json:"name"`
				Body string `json:"body"`
			} `json:"skills"`
		} `json:"catalog"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return err
	}
	if response.Type != "skillsCatalog" || len(response.Catalog.Skills) != 1 || response.Catalog.Skills[0].Name != "literal" || response.Catalog.Skills[0].Body != "Literal body" {
		return errors.New("daemon skills catalog mismatch")
	}
	if _, err := c.Request(ctx, map[string]any{"type": "setSkillsEnabled", "dirs": []string{dir}, "enabled": false}); err != nil {
		return err
	}
	if _, err := os.Stat(filepath.Join(dir, "SKILL.md.disabled")); err != nil {
		return err
	}
	return nil
}
