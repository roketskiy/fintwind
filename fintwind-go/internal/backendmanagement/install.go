package backendmanagement

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

type Run func(context.Context, string, []string, map[string]string, time.Duration) ([]byte, error)

// ManagedInstall is an explicit user operation. All package-manager state is
// confined to this profile, rather than the host's global installation.
func ManagedInstall(ctx context.Context, data, bun, version string, extra map[string]string, run Run) (string, error) {
	if !filepath.IsAbs(data) || !filepath.IsAbs(bun) || !SupportedVersion(version) {
		return "", errors.New("absolute manager paths and supported version required")
	}
	root := filepath.Join(data, "opencode-pm")
	home := filepath.Join(root, "bun")
	cache := filepath.Join(root, "bun-cache")
	handle, err := os.OpenRoot(data)
	if err != nil {
		return "", err
	}
	defer handle.Close()
	for _, rel := range []string{filepath.Join("opencode-pm", "bun"), filepath.Join("opencode-pm", "bun-cache")} {
		if err := handle.MkdirAll(rel, 0o700); err != nil {
			return "", err
		}
	}
	for _, path := range []string{root, home, cache} {
		canonical, err := filepath.EvalSymlinks(path)
		if err != nil {
			return "", err
		}
		relative, err := filepath.Rel(data, canonical)
		if err != nil || !filepath.IsLocal(relative) {
			return "", errors.New("managed installation escapes profile")
		}
	}
	environment := map[string]string{}
	for key, value := range extra {
		environment[key] = value
	}
	environment["BUN_INSTALL"] = home
	environment["BUN_INSTALL_CACHE_DIR"] = cache
	if _, err := run(ctx, bun, []string{"install", "-g", "--trust", "@opencode/cli@" + version}, environment, 240*time.Second); err != nil {
		return "", err
	}
	name := "opencode"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	binary := filepath.Join(home, "bin", name)
	canonical, err := filepath.EvalSymlinks(binary)
	if err != nil {
		return "", errors.New("installed OpenCode executable missing")
	}
	relative, err := filepath.Rel(home, canonical)
	if err != nil || !filepath.IsLocal(relative) {
		return "", errors.New("installed executable escapes profile")
	}
	info, err := os.Stat(canonical)
	if err != nil || !info.Mode().IsRegular() {
		return "", errors.New("installed OpenCode executable invalid")
	}
	output, err := run(ctx, canonical, []string{"--version"}, nil, 5*time.Second)
	if err != nil {
		return "", err
	}
	if ParseCLI(output) != version {
		return "", errors.New("installed OpenCode version did not match requested version")
	}
	return canonical, nil
}

func Owns(data, binary string) bool {
	canonical, err := filepath.EvalSymlinks(binary)
	if err != nil {
		return false
	}
	home := filepath.Join(data, "opencode-pm", "bun")
	packageRoot := filepath.Join(home, "install", "global", "node_modules", "@opencode", "cli")
	if info, err := os.Stat(filepath.Join(packageRoot, "package.json")); err != nil || !info.Mode().IsRegular() {
		return false
	}
	// Match only named package entry points, including their symlink targets.
	for _, name := range []string{"opencode", "opencode.exe"} {
		for _, candidate := range []string{filepath.Join(home, "bin", name), filepath.Join(packageRoot, "bin", name)} {
			resolved, err := filepath.EvalSymlinks(candidate)
			if err != nil {
				continue
			}
			if strings.EqualFold(filepath.Clean(resolved), filepath.Clean(canonical)) {
				relative, err := filepath.Rel(home, resolved)
				return err == nil && filepath.IsLocal(relative)
			}
		}
	}
	return false
}
