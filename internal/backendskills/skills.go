package backendskills

import (
	"context"
	"encoding/json"
	"errors"
	"hash/fnv"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"unicode/utf8"

	"fintwind-go/internal/backendstore"
)

type Service struct {
	root      string
	home      string
	daily     bool
	mu        sync.Mutex
	locations []location
}
type location struct {
	Source, Scope, Root string
	Project             *string
	ProjectRoot         string
}
type Install struct {
	Source    string `json:"source"`
	Dir       string `json:"dir"`
	SkillFile string `json:"skillFile"`
	Enabled   bool   `json:"enabled"`
}
type Entry struct {
	Name            string    `json:"name"`
	Description     string    `json:"description"`
	Scope           string    `json:"scope"`
	Project         *string   `json:"project"`
	ProjectRoot     string    `json:"projectRoot,omitempty"`
	Installs        []Install `json:"installs"`
	Enabled         bool      `json:"enabled"`
	AllowedTools    *string   `json:"allowedTools"`
	Body            string    `json:"body"`
	SupportingFiles int       `json:"supportingFiles"`
	TotalBytes      uint64    `json:"totalBytes"`
	ModifiedAt      *int64    `json:"modifiedAt"`
	Duplicates      int       `json:"duplicates"`
	RowKey          uint64    `json:"rowKey"`
}

func New(root, home string, daily bool) *Service {
	return &Service{root: root, home: home, daily: daily}
}
func within(root, path string) bool {
	rel, err := filepath.Rel(root, path)
	return err == nil && filepath.IsLocal(rel)
}

func (s *Service) Handle(ctx context.Context, c backendstore.Object) (backendstore.Object, bool, error) {
	kind := c.String("type")
	switch kind {
	case "loadSkills", "setSkillsEnabled", "trashSkills":
	default:
		return nil, false, nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := ctx.Err(); err != nil {
		return nil, true, err
	}
	if kind == "loadSkills" {
		locations := []location{{Source: "shared", Scope: "user", Root: filepath.Join(s.home, ".agents", "skills")}, {Source: "openCode", Scope: "user", Root: filepath.Join(s.home, ".config", "opencode", "skills")}}
		type projectLocation struct {
			Name string `json:"name"`
			Root string `json:"root"`
		}
		var projectRows []json.RawMessage
		if err := json.Unmarshal(c["projects"], &projectRows); err != nil {
			return nil, true, err
		}
		projects := make([]projectLocation, 0, len(projectRows))
		for _, raw := range projectRows {
			var project projectLocation
			if len(raw) > 0 && raw[0] == '[' {
				var pair []string
				if err := json.Unmarshal(raw, &pair); err != nil {
					return nil, true, err
				}
				if len(pair) != 2 {
					return nil, true, errors.New("skill project must contain name and root")
				}
				project.Name, project.Root = pair[0], pair[1]
			} else if err := json.Unmarshal(raw, &project); err != nil {
				return nil, true, err
			}
			projects = append(projects, project)
		}
		if len(projects) > 1000 {
			return nil, true, errors.New("too many skill projects")
		}
		for _, project := range projects {
			path, err := filepath.EvalSymlinks(project.Root)
			if err != nil {
				return nil, true, err
			}
			if !s.daily && !within(filepath.Join(s.root, "workspace"), path) {
				return nil, true, errors.New("skill project outside isolated workspace")
			}
			name := project.Name
			projectRoot := filepath.Clean(project.Root)
			locations = append(locations, location{Source: "shared", Scope: "project", Root: filepath.Join(path, ".agents", "skills"), Project: &name, ProjectRoot: projectRoot}, location{Source: "openCode", Scope: "project", Root: filepath.Join(path, ".opencode", "skills"), Project: &name, ProjectRoot: projectRoot})
		}
		entries, err := s.scan(ctx, locations)
		if err != nil {
			return nil, true, err
		}
		s.locations = locations
		return backendstore.Reply("skillsCatalog", backendstore.Fields(map[string]any{"catalog": map[string]any{"skills": entries}})), true, nil
	}
	var dirs []string
	if err := json.Unmarshal(c["dirs"], &dirs); err != nil {
		return nil, true, err
	}
	if len(dirs) > 500 {
		return nil, true, errors.New("too many skill directories")
	}
	// Validate every requested path before making the first mutation.
	for _, dir := range dirs {
		if _, err := s.skillPath(dir); err != nil {
			return nil, true, err
		}
	}
	for _, dir := range dirs {
		if err := ctx.Err(); err != nil {
			return nil, true, err
		}
		base, err := s.skillPath(dir)
		if err != nil {
			return nil, true, err
		}
		if kind == "trashSkills" {
			if err := trashDirectory(base); err != nil {
				return nil, true, err
			}
			continue
		}
		root, err := os.OpenRoot(base)
		if err != nil {
			return nil, true, err
		}
		from, to := "SKILL.md", "SKILL.md.disabled"
		if c.Bool("enabled") {
			from, to = to, from
			if info, e := root.Stat(to); e == nil && info.Mode().IsRegular() {
				root.Close()
				continue
			}
		}
		if _, err := root.Stat(from); errors.Is(err, os.ErrNotExist) {
			root.Close()
			continue
		}
		err = root.Rename(from, to)
		root.Close()
		if err != nil {
			return nil, true, err
		}
	}
	return backendstore.Reply("ack", nil), true, nil
}

func (s *Service) skillPath(path string) (string, error) {
	if !filepath.IsAbs(path) || strings.ContainsAny(path, "*?\x00") {
		return "", errors.New("absolute skill directory required")
	}
	canonical, err := filepath.EvalSymlinks(path)
	if err != nil {
		return "", err
	}
	if !s.daily && !within(s.root, canonical) {
		return "", errors.New("skill directory outside isolated profile")
	}
	locations := append([]location{{Root: filepath.Join(s.home, ".agents", "skills")}, {Root: filepath.Join(s.home, ".config", "opencode", "skills")}}, s.locations...)
	for _, location := range locations {
		base, err := filepath.EvalSymlinks(location.Root)
		if err != nil {
			continue
		}
		if !s.daily && !within(s.root, base) {
			continue
		}
		rel, err := filepath.Rel(base, canonical)
		if err == nil && rel != "." && filepath.IsLocal(rel) && !strings.ContainsAny(rel, "/\\") {
			root, err := os.OpenRoot(canonical)
			if err != nil {
				return "", err
			}
			defer root.Close()
			for _, name := range []string{"SKILL.md", "SKILL.md.disabled"} {
				if info, err := root.Stat(name); err == nil && info.Mode().IsRegular() {
					return canonical, nil
				}
			}
		}
	}
	return "", errors.New("directory is not a discovered skill install")
}

func (s *Service) scan(ctx context.Context, locations []location) ([]Entry, error) {
	result := []Entry{}
	identities := map[string]int{}
	count := 0
	for _, location := range locations {
		if count >= 500 {
			break
		}
		canonical, err := filepath.EvalSymlinks(location.Root)
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return nil, err
		}
		if !s.daily && !within(s.root, canonical) {
			return nil, errors.New("skill root outside isolated profile")
		}
		root, err := os.OpenRoot(canonical)
		if err != nil {
			return nil, err
		}
		entries, err := fs.ReadDir(root.FS(), ".")
		if err != nil {
			root.Close()
			return nil, err
		}
		for _, entry := range entries {
			if count >= 500 {
				break
			}
			if err := ctx.Err(); err != nil {
				root.Close()
				return nil, err
			}
			name := entry.Name()
			if strings.HasPrefix(name, ".") {
				continue
			}
			info, err := root.Stat(name)
			if err != nil || !info.IsDir() {
				continue
			}
			fileName := "SKILL.md"
			enabled := true
			file, err := root.Open(filepath.Join(name, fileName))
			if err != nil {
				fileName = "SKILL.md.disabled"
				enabled = false
				file, err = root.Open(filepath.Join(name, fileName))
			}
			if err != nil {
				continue
			}
			info, err = file.Stat()
			if err != nil || !info.Mode().IsRegular() || info.Size() > 256<<10 {
				file.Close()
				continue
			}
			raw, err := io.ReadAll(io.LimitReader(file, (256<<10)+1))
			file.Close()
			if err != nil || len(raw) > 256<<10 || !utf8.Valid(raw) {
				continue
			}
			count++
			front, body := frontmatter(string(raw))
			title := front["name"]
			if title == "" {
				title = name
			}
			if len(body) > 32<<10 {
				body = body[:32<<10]
				for !utf8.ValidString(body) {
					body = body[:len(body)-1]
				}
			}
			dir := filepath.Join(location.Root, name)
			install := Install{Source: location.Source, Dir: dir, SkillFile: filepath.Join(dir, fileName), Enabled: enabled}
			key := "user\x00" + title
			if location.Project != nil {
				key = "project\x00" + location.ProjectRoot + "\x00" + title
			}
			modified := info.ModTime().Unix()
			var allowed *string
			if value := front["allowed-tools"]; value != "" {
				allowed = &value
			}
			if index, ok := identities[key]; ok {
				existing := &result[index]
				existing.Installs = append(existing.Installs, install)
				existing.Enabled = existing.Enabled || enabled
				if existing.Description == "" {
					existing.Description = front["description"]
				}
				if existing.AllowedTools == nil {
					existing.AllowedTools = allowed
				}
				if existing.Body == "" {
					existing.Body = strings.TrimSpace(body)
				}
				if existing.ModifiedAt == nil || modified > *existing.ModifiedAt {
					existing.ModifiedAt = &modified
				}
			} else {
				files, bytes := measure(ctx, root, name, 0, new(int))
				identities[key] = len(result)
				result = append(result, Entry{Name: title, Description: front["description"], Scope: location.Scope, Project: location.Project, ProjectRoot: location.ProjectRoot, Installs: []Install{install}, Enabled: enabled, AllowedTools: allowed, Body: strings.TrimSpace(body), SupportingFiles: max(0, files-1), TotalBytes: bytes, ModifiedAt: &modified})
			}
		}
		root.Close()
	}
	sort.SliceStable(result, func(i, j int) bool {
		a, b := result[i], result[j]
		if a.Project == nil && b.Project != nil {
			return true
		}
		if a.Project != nil && b.Project == nil {
			return false
		}
		if a.Project != nil && b.Project != nil && *a.Project != *b.Project {
			return *a.Project < *b.Project
		}
		return strings.ToLower(a.Name) < strings.ToLower(b.Name)
	})
	counts := map[string]int{}
	for _, entry := range result {
		counts[entry.Name]++
	}
	for index := range result {
		entry := &result[index]
		entry.Duplicates = counts[entry.Name] - 1
		hash := fnv.New64a()
		for _, install := range entry.Installs {
			_, _ = io.WriteString(hash, install.Dir)
			if install.Enabled {
				_, _ = hash.Write([]byte{1})
			} else {
				_, _ = hash.Write([]byte{0})
			}
		}
		entry.RowKey = hash.Sum64()
	}
	return result, ctx.Err()
}

func frontmatter(text string) (map[string]string, string) {
	front := map[string]string{}
	if !strings.HasPrefix(text, "---") {
		return front, text
	}
	block, body, ok := strings.Cut(strings.TrimPrefix(text, "---"), "\n---")
	if !ok {
		return front, text
	}
	for _, line := range strings.Split(block, "\n") {
		key, value, ok := strings.Cut(line, ":")
		if ok {
			front[strings.TrimSpace(key)] = strings.Trim(strings.TrimSpace(value), "\"'")
		}
	}
	return front, strings.TrimSpace(strings.TrimLeft(body, "-"))
}

func measure(ctx context.Context, root *os.Root, path string, depth int, count *int) (int, uint64) {
	if depth > 6 || *count >= 500 || ctx.Err() != nil {
		return 0, 0
	}
	entries, err := fs.ReadDir(root.FS(), filepath.ToSlash(path))
	if err != nil {
		return 0, 0
	}
	files := 0
	var bytes uint64
	for _, entry := range entries {
		if *count >= 500 || ctx.Err() != nil {
			break
		}
		if strings.HasPrefix(entry.Name(), ".") {
			continue
		}
		child := filepath.Join(path, entry.Name())
		info, err := root.Stat(child)
		if err != nil {
			continue
		}
		if info.IsDir() {
			n, b := measure(ctx, root, child, depth+1, count)
			files += n
			bytes += b
		} else if info.Mode().IsRegular() {
			*count++
			files++
			bytes += uint64(info.Size())
		}
	}
	return files, bytes
}
