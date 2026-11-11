// Package launchprofile builds isolated launch plans for the MyGo prototype.
package launchprofile

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
)

type Profile string

const (
	Daily Profile = "daily"
	Agent Profile = "agent"
)

// Options contains only caller-supplied inputs. Resolve reads no process state
// and performs no filesystem writes.
type Options struct {
	Profile   string
	Root      string
	Name      string
	ParentEnv []string
}

// Plan is a fully resolved, serializable launch description.
type Plan struct {
	Profile        Profile  `json:"profile"`
	Name           string   `json:"name"`
	DataDir        string   `json:"dataDir"`
	PreferencesDir string   `json:"preferencesDir"`
	WorkDir        string   `json:"workDir"`
	BuildDir       string   `json:"buildDir"`
	Environment    []string `json:"-"`
	AppArgs        []string `json:"appArgs"`

	prepareDirs     []string
	prepareRoot     string
	preferencesRoot string
}

var safeAgentEnvironment = map[string]struct{}{
	"COMSPEC": {}, "PATH": {},
	"PATHEXT": {}, "PROCESSOR_ARCHITECTURE": {}, "PROCESSOR_IDENTIFIER": {},
	"PROCESSOR_LEVEL": {}, "PROCESSOR_REVISION": {}, "PROGRAMFILES": {},
	"PROGRAMFILES(X86)": {}, "SYSTEMDRIVE": {}, "SYSTEMROOT": {}, "WINDIR": {},
	"HTTP_PROXY": {}, "HTTPS_PROXY": {}, "NO_PROXY": {}, "ALL_PROXY": {},
}

// Resolve converts external launcher inputs into a validated launch plan.
func Resolve(options Options) (Plan, error) {
	profile := Profile(strings.ToLower(strings.TrimSpace(options.Profile)))
	if profile == "" {
		profile = Agent
	}
	if profile != Daily && profile != Agent {
		return Plan{}, fmt.Errorf("unknown launch profile %q", options.Profile)
	}
	if strings.TrimSpace(options.Root) == "" {
		return Plan{}, errors.New("repository root is required")
	}
	root, err := filepath.Abs(options.Root)
	if err != nil {
		return Plan{}, fmt.Errorf("resolve repository root: %w", err)
	}
	root, err = filepath.EvalSymlinks(root)
	if err != nil {
		return Plan{}, fmt.Errorf("resolve repository root: %w", err)
	}
	info, err := os.Stat(root)
	if err != nil || !info.IsDir() {
		return Plan{}, fmt.Errorf("repository root is not a directory: %s", root)
	}

	name := strings.TrimSpace(options.Name)
	if name == "" {
		name = "default"
	}
	if profile == Agent && !validName(name) {
		return Plan{}, errors.New("agent name must be 1-64 ASCII letters, digits, hyphens, or underscores")
	}
	if profile == Daily {
		name = "daily"
	}

	env, err := parseEnvironment(options.ParentEnv)
	if err != nil {
		return Plan{}, err
	}
	localAppData := lookup(env, "LOCALAPPDATA")
	if localAppData == "" {
		return Plan{}, errors.New("LOCALAPPDATA is required")
	}
	localAppData, err = filepath.Abs(localAppData)
	if err != nil {
		return Plan{}, fmt.Errorf("resolve LOCALAPPDATA: %w", err)
	}
	if resolved, resolveErr := filepath.EvalSymlinks(localAppData); resolveErr == nil {
		localAppData = resolved
	} else if !os.IsNotExist(resolveErr) {
		return Plan{}, fmt.Errorf("resolve LOCALAPPDATA: %w", resolveErr)
	}

	plan := Plan{Profile: profile, Name: name}
	switch profile {
	case Daily:
		plan.DataDir = filepath.Join(localAppData, "Fintwind")
		plan.PreferencesDir = filepath.Join(localAppData, "Fintwind Go")
		plan.WorkDir = root
		plan.BuildDir = filepath.Join(root, "target", "mygo", "daily")
		if err := validateContainedPath(root, plan.BuildDir); err != nil {
			return Plan{}, fmt.Errorf("daily build directory: %w", err)
		}
		plan.prepareDirs = []string{plan.PreferencesDir, plan.BuildDir}
		plan.prepareRoot = root
		plan.preferencesRoot = localAppData
		plan.Environment = dailyEnvironment(env)
		plan.AppArgs = []string{"--profile", "daily", "--data-dir", plan.DataDir, "--preferences-dir", plan.PreferencesDir}
	case Agent:
		base := filepath.Join(localAppData, "Fintwind Go", "Agent", name)
		if err := validateContainedPath(localAppData, base); err != nil {
			return Plan{}, fmt.Errorf("agent root: %w", err)
		}
		protected := []string{filepath.Join(localAppData, "Fintwind")}
		if profileHome := userProfile(env); profileHome != "" {
			protected = append(protected, filepath.Join(profileHome, ".fintwind"))
		}
		if err := rejectOverlap(base, protected...); err != nil {
			return Plan{}, fmt.Errorf("agent home: %w", err)
		}
		home := filepath.Join(base, "home")
		config := filepath.Join(base, "config")
		tmp := filepath.Join(base, "tmp")
		workspace := filepath.Join(base, "workspace")
		data := filepath.Join(base, "data")
		plan.DataDir = data
		plan.PreferencesDir = filepath.Join(config, "fintwind")
		plan.WorkDir = workspace
		plan.BuildDir = filepath.Join(workspace, "build")
		plan.prepareDirs = []string{home, config, tmp, workspace, data, plan.PreferencesDir, filepath.Join(config, "opencode"), plan.BuildDir}
		plan.prepareRoot = localAppData
		for _, path := range plan.prepareDirs {
			if err := validateContainedPath(base, path); err != nil {
				return Plan{}, fmt.Errorf("agent path: %w", err)
			}
		}
		plan.Environment = agentEnvironment(env, home, config, tmp, data, profile)
		plan.AppArgs = []string{"--profile", string(profile), "--data-dir", plan.DataDir, "--preferences-dir", plan.PreferencesDir}
	}
	return plan, nil
}

// Prepare creates only the directories owned by this profile. Daily's
// production DataDir is intentionally excluded from prepareDirs.
func (plan Plan) Prepare() error {
	for _, dir := range plan.prepareDirs {
		root := plan.prepareRoot
		if plan.Profile == Daily && dir == plan.PreferencesDir {
			root = plan.preferencesRoot
		}
		if err := validateContainedPath(root, dir); err != nil {
			return fmt.Errorf("unsafe launch directory %s: %w", dir, err)
		}
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return fmt.Errorf("create launch directory %s: %w", dir, err)
		}
	}
	return nil
}

func validName(name string) bool {
	if len(name) == 0 || len(name) > 64 {
		return false
	}
	for _, r := range name {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '-' || r == '_') {
			return false
		}
	}
	return true
}

func parseEnvironment(values []string) (map[string]string, error) {
	env := make(map[string]string, len(values))
	for _, value := range values {
		key, val, ok := strings.Cut(value, "=")
		if key == "" && strings.HasPrefix(value, "=") {
			continue
		} // Windows drive-current-directory entries.
		if !ok || key == "" {
			return nil, fmt.Errorf("invalid environment entry %q", value)
		}
		env[envKey(key)] = val
	}
	return env, nil
}

func envKey(key string) string {
	if runtime.GOOS == "windows" {
		return strings.ToUpper(key)
	}
	return key
}

func lookup(env map[string]string, key string) string { return env[envKey(key)] }

func dailyEnvironment(parent map[string]string) []string {
	result := make([]string, 0, len(parent)+1)
	keys := make([]string, 0, len(parent))
	for key := range parent {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, key := range keys {
		value := parent[key]
		if strings.HasPrefix(strings.ToUpper(key), "FINTWIND_") {
			continue
		}
		result = append(result, key+"="+value)
	}
	result = append(result, "FINTWIND_MYGO_PROFILE=daily")
	return result
}

func agentEnvironment(parent map[string]string, home, config, tmp, data string, profile Profile) []string {
	result := make([]string, 0, len(parent)+12)
	keys := make([]string, 0, len(parent))
	for key := range parent {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, key := range keys {
		value := parent[key]
		upper := strings.ToUpper(key)
		if _, ok := safeAgentEnvironment[upper]; ok {
			result = append(result, key+"="+value)
		}
	}
	set := func(key, value string) { result = append(result, key+"="+value) }
	set("HOME", home)
	set("USERPROFILE", home)
	set("HOMEDRIVE", filepath.VolumeName(home))
	set("HOMEPATH", strings.TrimPrefix(home, filepath.VolumeName(home)))
	set("APPDATA", filepath.Join(config, "roaming"))
	set("LOCALAPPDATA", filepath.Join(config, "local"))
	set("XDG_CONFIG_HOME", filepath.Join(config, "xdg"))
	set("XDG_DATA_HOME", filepath.Join(data, "xdg"))
	set("XDG_STATE_HOME", filepath.Join(data, "state"))
	set("XDG_CACHE_HOME", filepath.Join(data, "cache"))
	set("TEMP", tmp)
	set("TMP", tmp)
	set("OPENCODE_CONFIG_DIR", filepath.Join(config, "opencode"))
	set("OPENCODE_TEST_HOME", home)
	set("OPENCODE_DB", filepath.Join(data, "opencode.db"))
	set("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
	set("FINTWIND_MYGO_PROFILE", string(profile))
	return result
}

func userProfile(env map[string]string) string {
	if value := lookup(env, "USERPROFILE"); value != "" {
		return value
	}
	return lookup(env, "HOME")
}

func rejectOverlap(candidate string, protected ...string) error {
	for _, path := range protected {
		if path == "" {
			continue
		}
		candidateAbs, err := filepath.Abs(candidate)
		if err != nil {
			return err
		}
		protectedAbs, err := filepath.Abs(path)
		if err != nil {
			return err
		}
		if within(candidateAbs, protectedAbs) || within(protectedAbs, candidateAbs) {
			return fmt.Errorf("path %s overlaps protected path %s", candidateAbs, protectedAbs)
		}
	}
	return nil
}

func validateContainedPath(base, candidate string) error {
	baseAbs, err := filepath.Abs(base)
	if err != nil {
		return err
	}
	candidateAbs, err := filepath.Abs(candidate)
	if err != nil {
		return err
	}
	if !within(candidateAbs, baseAbs) {
		return fmt.Errorf("%s escapes %s", candidateAbs, baseAbs)
	}
	rel, err := filepath.Rel(baseAbs, candidateAbs)
	if err != nil {
		return err
	}
	current := baseAbs
	if info, statErr := os.Lstat(current); statErr == nil && info.Mode()&os.ModeSymlink != 0 {
		return fmt.Errorf("path ancestor is a symlink: %s", current)
	}
	for _, part := range strings.Split(rel, string(filepath.Separator)) {
		if part == "." || part == "" {
			continue
		}
		current = filepath.Join(current, part)
		info, statErr := os.Lstat(current)
		if os.IsNotExist(statErr) {
			break
		}
		if statErr != nil {
			return statErr
		}
		if info.Mode()&os.ModeSymlink != 0 {
			return fmt.Errorf("path ancestor is a symlink: %s", current)
		}
	}
	return nil
}

func within(path, base string) bool {
	rel, err := filepath.Rel(base, path)
	if err != nil {
		return false
	}
	return rel == "." || rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}
