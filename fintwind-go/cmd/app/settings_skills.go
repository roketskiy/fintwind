package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// skillSourceLabel mirrors SkillSource::label: the shared tree keeps its
// translated name, the OpenCode tree keeps its product name.
func skillSourceLabel(source string) string {
	switch strings.ToLower(strings.TrimSpace(source)) {
	case "shared":
		return tr("Shared")
	case "opencode":
		return "OpenCode"
	default:
		return source
	}
}

// skillSourcesLabel mirrors SkillEntry::sources_label: one label per install
// joined with middle dots.
func skillSourcesLabel(skill settingsSkill) string {
	labels := make([]string, 0, len(skill.Installs))
	for _, install := range skill.Installs {
		labels = append(labels, skillSourceLabel(install.Source))
	}
	return strings.Join(labels, " · ")
}

// skillIconName mirrors GPUI skill_icon: a grouped skill sits on the
// package tile, otherwise the tile follows its single source.
func skillIconName(skill settingsSkill) string {
	if len(skill.Installs) > 1 {
		return "package"
	}
	if len(skill.Installs) == 1 && strings.EqualFold(skill.Installs[0].Source, "opencode") {
		return "provider-opencode"
	}
	return "package"
}

// skillMatches mirrors GPUI skill_matches: one lowercased substring query
// over the name, description, install source labels, and project.
func skillMatches(skill settingsSkill, query string) bool {
	q := strings.ToLower(strings.TrimSpace(query))
	if q == "" {
		return true
	}
	if strings.Contains(strings.ToLower(skill.Name), q) {
		return true
	}
	if strings.Contains(strings.ToLower(skill.Description), q) {
		return true
	}
	if strings.Contains(strings.ToLower(skillSourcesLabel(skill)), q) {
		return true
	}
	return strings.Contains(strings.ToLower(skill.Project), q)
}

// skillsVisible filters the catalog in catalog order: a grouped skill passes
// the source filter when any of its copies lives in that source's tree.
func skillsVisible(skills []settingsSkill, query, source string) []settingsSkill {
	visible := make([]settingsSkill, 0, len(skills))
	for _, skill := range skills {
		if source != "" {
			matched := false
			for _, install := range skill.Installs {
				if strings.EqualFold(install.Source, source) {
					matched = true
					break
				}
			}
			if !matched {
				continue
			}
		}
		if !skillMatches(skill, query) {
			continue
		}
		visible = append(visible, skill)
	}
	return visible
}

// skillsSourceCounts counts distinct skills per install source so the filter
// menu never surprises with an empty pick.
func skillsSourceCounts(skills []settingsSkill) map[string]int {
	counts := map[string]int{}
	for _, skill := range skills {
		seen := map[string]bool{}
		for _, install := range skill.Installs {
			key := strings.ToLower(strings.TrimSpace(install.Source))
			if key == "" || seen[key] {
				continue
			}
			seen[key] = true
			counts[key]++
		}
	}
	return counts
}

// skillProjectLabel groups the roster: a project name, else User skills.
func skillProjectLabel(skill settingsSkill) string {
	if strings.TrimSpace(skill.Project) != "" {
		return skill.Project
	}
	return tr("User skills")
}

// skillScopeCaption mirrors the detail caption: sources plus the scope.
func skillScopeCaption(skill settingsSkill) string {
	scope := tr("available in every project")
	if strings.TrimSpace(skill.Project) != "" {
		scope = strings.ReplaceAll(tr("in %{project}"), "%{project}", skill.Project)
		if scope == "in %{project}" {
			scope = "in " + skill.Project
		}
	}
	sources := skillSourcesLabel(skill)
	if sources == "" {
		return scope
	}
	return sources + " · " + scope
}

// formatSkillBytes mirrors GPUI format_bytes: compact B/KB/MB.
func formatSkillBytes(bytes uint64) string {
	if bytes < 1024 {
		return fmt.Sprintf("%d B", bytes)
	}
	if bytes < 1024*1024 {
		return fmt.Sprintf("%.1f KB", float64(bytes)/1024)
	}
	return fmt.Sprintf("%.1f MB", float64(bytes)/(1024*1024))
}

// skillUpdatedLabel mirrors GPUI updated_label: bare relative time.
func skillUpdatedLabel(modifiedAt *uint64) string {
	if modifiedAt == nil {
		return ""
	}
	now := uint64(time.Now().Unix())
	elapsed := uint64(0)
	if now > *modifiedAt {
		elapsed = now - *modifiedAt
	}
	switch {
	case elapsed < 90:
		return tr("Just now")
	case elapsed < 3600:
		return strings.ReplaceAll(tr("%{count}m ago"), "%{count}", fmt.Sprint(elapsed/60))
	case elapsed < 86400:
		return strings.ReplaceAll(tr("%{count}h ago"), "%{count}", fmt.Sprint(elapsed/3600))
	default:
		return strings.ReplaceAll(tr("%{count}d ago"), "%{count}", fmt.Sprint(elapsed/86400))
	}
}

// skillContentsLabel mirrors the detail Contents row.
func skillContentsLabel(skill settingsSkill) string {
	parts := []string{}
	if skill.SupportingFiles == 1 {
		parts = append(parts, tr("1 supporting file"))
	} else if skill.SupportingFiles > 1 {
		parts = append(parts, strings.ReplaceAll(tr("%{count} supporting files"), "%{count}", fmt.Sprint(skill.SupportingFiles)))
	}
	parts = append(parts, formatSkillBytes(skill.TotalBytes))
	return strings.Join(parts, " · ")
}

// skillDuplicateLabel mirrors the detail duplicates warning.
func skillDuplicateLabel(duplicates int) string {
	if duplicates == 1 {
		return tr("Same name in 1 other location — the most specific copy wins")
	}
	return strings.ReplaceAll(tr("Same name in %{count} other locations — the most specific copy wins"), "%{count}", fmt.Sprint(duplicates))
}

// skillCompactDir shortens a location line the way the detail pane does:
// the home directory collapses to ~, everything else stays verbatim.
func skillCompactDir(dir string) string {
	if dir == "" {
		return dir
	}
	if home, err := os.UserHomeDir(); err == nil && home != "" {
		if dir == home {
			return "~"
		}
		if strings.HasPrefix(dir, home+string(filepath.Separator)) {
			return "~" + strings.TrimPrefix(dir, home)
		}
	}
	return dir
}

// skillsEffectiveSelection keeps the detail pane populated while skills
// exist: the stored selection wins when still visible, else the first
// visible row.
func skillsEffectiveSelection(visible []settingsSkill, selected string) *settingsSkill {
	for i := range visible {
		if visible[i].key() == selected {
			return &visible[i]
		}
	}
	if len(visible) > 0 {
		return &visible[0]
	}
	return nil
}
