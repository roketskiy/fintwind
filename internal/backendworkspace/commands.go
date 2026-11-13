package backendworkspace

import (
	"context"
	"encoding/json"
	"fmt"
	"path/filepath"
	"sort"
	"strings"
)

const (
	commandScanCap   = 500
	commandMaxBytes  = 64 * 1024
	commandNameDepth = 3
)

// Command scopes use the exact protocol spelling (no case folding:
// the protocol enum has no rename rule, so "Project", not "project").
const (
	scopeProject = "Project"
	scopeUser    = "User"
	scopeSkill   = "Skill"
	scopeBuiltin = "Builtin"
)

type discoverSlashCommandsOp struct {
	Type        string  `json:"type"`
	ProjectRoot string  `json:"projectRoot"`
	Binary      *string `json:"binary"`
}

type slashCommand struct {
	Name         string  `json:"name"`
	Description  string  `json:"description"`
	Scope        string  `json:"scope"`
	ArgumentHint *string `json:"argumentHint"`
	Template     *string `json:"template"`
}

type SlashCommand = slashCommand

// MergeRegisteredCommands retains locally expanded templates while exposing
// commands and exact skill IDs only the live OpenCode registry can know.
func MergeRegisteredCommands(local []SlashCommand, reported, skills []SlashCommand, skillsReady bool) []SlashCommand {
	merged := append([]SlashCommand{}, local...)
	for _, report := range reported {
		found := false
		for i := range merged {
			if merged[i].Name == report.Name {
				if merged[i].Description == "" {
					merged[i].Description = report.Description
				}
				found = true
				break
			}
		}
		if !found {
			report.Scope = scopeBuiltin
			report.Template = nil
			report.ArgumentHint = nil
			merged = append(merged, report)
		}
	}
	if skillsReady {
		retained := merged[:0]
		for _, command := range merged {
			if command.Scope != scopeSkill {
				retained = append(retained, command)
			}
		}
		merged = retained
		for _, skill := range skills {
			skill.Scope = scopeSkill
			skill.Template = nil
			skill.ArgumentHint = nil
			merged = append(merged, skill)
		}
	}
	sort.SliceStable(merged, func(i, j int) bool {
		if scopeDisplayRank(merged[i].Scope) != scopeDisplayRank(merged[j].Scope) {
			return scopeDisplayRank(merged[i].Scope) < scopeDisplayRank(merged[j].Scope)
		}
		return merged[i].Name < merged[j].Name
	})
	return merged
}

type slashCommandsResult struct {
	Type     string         `json:"type"`
	Commands []slashCommand `json:"commands"`
}

func dispatchDiscoverSlashCommands(ctx context.Context, root *confinedRoot, operation json.RawMessage) (json.RawMessage, error) {
	defer root.close()
	var op discoverSlashCommandsOp
	if err := decodeOperation(operation, &op, opDiscoverSlashCommands); err != nil {
		return nil, err
	}
	if op.Binary != nil && strings.TrimSpace(*op.Binary) != "" {
		return nil, fmt.Errorf("discoverSlashCommands with binary needs the live server registry, which the local module never contacts")
	}
	baseRel, err := scopedBaseRel(root, op.ProjectRoot)
	if err != nil {
		return nil, err
	}
	commands := []slashCommand{}
	project := ifEmpty(baseRel, ".")
	scanCommandFiles(ctx, root, filepath.Join(project, ".opencode", "commands"), scopeProject, &commands)
	scanCommandFiles(ctx, root, filepath.Join(project, ".opencode", "command"), scopeProject, &commands)
	scanSkillFiles(ctx, root, filepath.Join(project, ".opencode", "skills"), &commands)
	scanSkillFiles(ctx, root, filepath.Join(project, ".claude", "skills"), &commands)
	scanSkillFiles(ctx, root, filepath.Join(project, ".agents", "skills"), &commands)
	scanCommandFiles(ctx, root, filepath.Join(project, ".fintwind", "commands"), scopeProject, &commands)
	// User-scope sources live outside the isolation root and are
	// deliberately excluded; builtins still apply.
	commands = append(commands, builtinCommands()...)
	commands = dedupAndSortCommands(commands)
	if commands == nil {
		commands = []slashCommand{}
	}
	return encodeResult(slashCommandsResult{Type: "slashCommands", Commands: commands})
}

// scanCommandFiles collects "*.md" command files. Subdirectories namespace
// the name ("a/b.md" becomes "a/b").
func scanCommandFiles(ctx context.Context, root *confinedRoot, dir, scope string, out *[]slashCommand) {
	type frame struct {
		rel       string
		namespace []string
	}
	stack := []frame{{rel: dir}}
	for len(stack) > 0 {
		if ctx.Err() != nil || len(*out) >= commandScanCap {
			return
		}
		top := stack[len(stack)-1]
		stack = stack[:len(stack)-1]
		if len(top.namespace) > commandNameDepth {
			continue
		}
		handle, err := root.handle.Open(top.rel)
		if err != nil {
			continue
		}
		items, err := handle.ReadDir(-1)
		closeErr := handle.Close()
		if err != nil {
			continue
		}
		if closeErr != nil {
			return
		}
		for _, item := range items {
			if len(*out) >= commandScanCap {
				return
			}
			name := item.Name()
			if strings.HasPrefix(name, ".") {
				continue
			}
			rel := filepath.Join(top.rel, name)
			stat, err := root.handle.Stat(rel)
			if err != nil {
				continue
			}
			if stat.IsDir() {
				namespace := append(append([]string{}, top.namespace...), name)
				stack = append(stack, frame{rel: rel, namespace: namespace})
				continue
			}
			stem, ok := strings.CutSuffix(name, ".md")
			if !ok {
				continue
			}
			contents, ok := readSmallCommandFile(root, rel)
			if !ok {
				continue
			}
			front := parseFrontmatter(contents)
			full := strings.Join(append(append([]string{}, top.namespace...), stem), "/")
			description := front.description
			if description == "" {
				description = firstLineSummary(front.body)
			}
			var argumentHint *string
			if front.argumentHint != "" {
				hint := front.argumentHint
				argumentHint = &hint
			}
			body := strings.TrimSpace(front.body)
			template := body
			*out = append(*out, slashCommand{
				Name:         full,
				Description:  description,
				Scope:        scope,
				ArgumentHint: argumentHint,
				Template:     &template,
			})
		}
	}
}

// scanSkillFiles collects fallback skill IDs: directory names carrying a
// SKILL.md file. Skill contents never become templates here.
func scanSkillFiles(ctx context.Context, root *confinedRoot, dir string, out *[]slashCommand) {
	handle, err := root.handle.Open(dir)
	if err != nil {
		return
	}
	items, err := handle.ReadDir(-1)
	closeErr := handle.Close()
	if err != nil || closeErr != nil {
		return
	}
	for _, item := range items {
		if ctx.Err() != nil || len(*out) >= commandScanCap {
			return
		}
		name := item.Name()
		if strings.HasPrefix(name, ".") {
			continue
		}
		rel := filepath.Join(dir, name)
		stat, err := root.handle.Stat(rel)
		if err != nil || !stat.IsDir() {
			continue
		}
		contents, ok := readSmallCommandFile(root, filepath.Join(rel, "SKILL.md"))
		if !ok {
			continue
		}
		front := parseFrontmatter(contents)
		*out = append(*out, slashCommand{
			Name:        name,
			Description: front.description,
			Scope:       scopeSkill,
		})
	}
}

func readSmallCommandFile(root *confinedRoot, rel string) (string, bool) {
	info, err := root.handle.Stat(rel)
	if err != nil || info.IsDir() || info.Size() > commandMaxBytes {
		return "", false
	}
	data, err := root.readRel(context.Background(), rel)
	if err != nil {
		return "", false
	}
	return data, true
}

type frontmatter struct {
	description  string
	argumentHint string
	body         string
}

// parseFrontmatter reads the flat "key: value" YAML header the picker
// shows, without a YAML parser: only description and argument-hint
// matter, and an unparsed key must not cost the command its listing.
func parseFrontmatter(contents string) frontmatter {
	front := frontmatter{body: contents}
	rest, ok := strings.CutPrefix(contents, "---")
	if !ok {
		return front
	}
	block, body, ok := strings.Cut(rest, "\n---")
	if !ok {
		return front
	}
	front.body = strings.TrimLeft(strings.TrimLeft(body, "-"), " \t\r\n")
	for _, line := range strings.Split(block, "\n") {
		key, value, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		value = strings.Trim(strings.TrimSpace(value), "\"'")
		if value == "" {
			continue
		}
		switch strings.TrimSpace(key) {
		case "description":
			front.description = value
		case "argument-hint":
			front.argumentHint = value
		}
	}
	return front
}

// firstLineSummary glosses a command file without a description from its
// opening prose, clipped to one row.
func firstLineSummary(body string) string {
	for _, line := range strings.Split(body, "\n") {
		trimmed := strings.TrimSpace(line)
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}
		runes := []rune(trimmed)
		if len(runes) > 100 {
			return string(runes[:100]) + "…"
		}
		return trimmed
	}
	return ""
}

// builtinCommands is the fixed built-in set. Descriptions fall back to
// plain English; this module owns no localized catalog.
func builtinCommands() []slashCommand {
	template := func(value string) *string { return &value }
	return []slashCommand{
		{
			Name:        "init",
			Description: "Analyze this repository and write contributor instructions for coding agents.",
			Scope:       scopeBuiltin,
			Template: template("Analyze this repository and write AGENTS.md for coding agents " +
				"working in it: build, test and lint commands, architecture overview, code " +
				"conventions, and any gotchas. If the file already exists, update it and " +
				"keep it concise."),
		},
		{
			Name:        "review",
			Description: "Review the pending changes in this working tree.",
			Scope:       scopeBuiltin,
			Template: template("Review the pending changes in this working tree: uncommitted work plus commits " +
				"not on the default branch. Look for bugs, regressions, and risky patterns; " +
				"report findings ordered by severity with file and line references."),
		},
		{
			Name:        "commit",
			Description: "Stage the appropriate files and create a git commit.",
			Scope:       scopeBuiltin,
			Template: template("Look at the current working tree, stage the appropriate files, and create a git " +
				"commit with a clear message that describes the change and why it was made."),
		},
		{Name: "compact", Description: "Compact the session context.", Scope: scopeBuiltin},
		{Name: "reload-server", Description: "Hot-reload the native OpenCode server.", Scope: scopeBuiltin},
	}
}

// scopeResolutionRank follows the protocol declaration order, which is
// the resolution precedence and intentionally differs from display order.
func scopeResolutionRank(scope string) int {
	switch scope {
	case scopeProject:
		return 0
	case scopeUser:
		return 1
	case scopeSkill:
		return 2
	default:
		return 3
	}
}

func scopeDisplayRank(scope string) int {
	switch scope {
	case scopeBuiltin:
		return 0
	case scopeProject:
		return 1
	case scopeUser:
		return 2
	default:
		return 3
	}
}

// dedupAndSortCommands keeps the most specific scope on a name collision,
// then orders builtins first and the rest alphabetically per scope.
func dedupAndSortCommands(commands []slashCommand) []slashCommand {
	sorted := append([]slashCommand{}, commands...)
	sort.Slice(sorted, func(i, j int) bool {
		if scopeResolutionRank(sorted[i].Scope) != scopeResolutionRank(sorted[j].Scope) {
			return scopeResolutionRank(sorted[i].Scope) < scopeResolutionRank(sorted[j].Scope)
		}
		return sorted[i].Name < sorted[j].Name
	})
	seen := map[string]bool{}
	unique := sorted[:0]
	for _, command := range sorted {
		if seen[command.Name] {
			continue
		}
		seen[command.Name] = true
		unique = append(unique, command)
	}
	sort.Slice(unique, func(i, j int) bool {
		if scopeDisplayRank(unique[i].Scope) != scopeDisplayRank(unique[j].Scope) {
			return scopeDisplayRank(unique[i].Scope) < scopeDisplayRank(unique[j].Scope)
		}
		return unique[i].Name < unique[j].Name
	})
	return unique
}
