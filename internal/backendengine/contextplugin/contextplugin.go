// Package contextplugin ships the OpenCode server plugin that feeds the
// context composition panel: it hooks session.context and writes a token
// estimate snapshot per assembled primary request.
package contextplugin

import (
	_ "embed"
	"os"
	"path/filepath"
)

//go:embed fintwind-context.ts
var source []byte

const fileName = "fintwind-context.ts"

// Materialize writes the plugin into the engine config directory's plugins
// folder so OpenCode auto-discovers it. The plugin stays inert unless the
// engine runs with FINTWIND_CONTEXT_DIR, so sharing a config directory with
// direct OpenCode usage is safe.
func Materialize(configDir string) error {
	plugins := filepath.Join(configDir, "plugins")
	if err := os.MkdirAll(plugins, 0o700); err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(plugins, fileName), source, 0o600)
}

// ResolveConfigDir mirrors the engine's global config root: the isolated
// profile keeps its private home, the daily profile uses OPENCODE_CONFIG_DIR
// or the user's OpenCode config directory.
func ResolveConfigDir(daily bool, root string) (string, error) {
	if !daily {
		return filepath.Join(root, "home", "opencode"), nil
	}
	if dir := os.Getenv("OPENCODE_CONFIG_DIR"); dir != "" {
		return dir, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".config", "opencode"), nil
}
