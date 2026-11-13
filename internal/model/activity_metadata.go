package model

import (
	"encoding/json"
	"fmt"
	"net/url"
	"sort"
	"strings"
)

const activityTextLimit = 16000

func activityLimit(text string, n int) string {
	r := []rune(strings.TrimSpace(text))
	if len(r) > n {
		return string(r[:n]) + "\n[truncated]"
	}
	return string(r)
}
func activityObject(value any) map[string]any { v, _ := value.(map[string]any); return v }
func activityString(value any) string         { v, _ := value.(string); return v }
func decodeActivity(raw json.RawMessage) any {
	var value any
	if len(raw) > 8<<20 || json.Unmarshal(raw, &value) != nil {
		return nil
	}
	return value
}

// ApplyToolMetadata only projects public activity fields. It never forwards
// provider metadata wholesale, and never creates activities for nested calls.
func ApplyToolMetadata(item *ActivityItem, input, output, source json.RawMessage) {
	arguments, result, metadata := decodeActivity(input), decodeActivity(output), decodeActivity(source)
	if arguments != nil {
		raw, _ := json.MarshalIndent(arguments, "", "  ")
		value := activityLimit(string(raw), activityTextLimit)
		item.Arguments = &value
	}
	if result != nil {
		value := activityLimit(activityOutput(result, 0), activityTextLimit)
		item.Output = &value
		if item.Failed {
			for _, line := range strings.Split(value, "\n") {
				if line = strings.TrimSpace(line); line != "" {
					item.Detail = &line
					break
				}
			}
		}
	}
	for _, value := range []any{arguments, metadata} {
		if summary := toolCallSummary(value, 0); summary != "" {
			target := compactActivity(summary)
			item.DisplayTarget = &target
		} else if target := activityTarget(item.Kind, value); target != "" {
			text := compactActivity(target)
			item.DisplayTarget = &text
		}
		if item.Kind == KindCommand {
			if text := findActivityString(value, []string{"description"}, 0); text != "" {
				text = compactActivity(text)
				item.DisplayDescription = &text
			}
		}
		if item.Kind == KindFileChange {
			changes := []ActivityFileChange{}
			extractActivityChanges(value, &changes, 0)
			if len(changes) > 0 {
				item.FileChanges = changes
			}
		}
	}
	images := append([]string{}, item.ImageURLs...)
	collectActivityImages(result, &images, 0)
	collectActivityImages(metadata, &images, 0)
	unique := []string{}
	seen := map[string]bool{}
	bytes := 0
	for _, image := range images {
		if seen[image] || len(image) > 2<<20 || len(unique) >= 32 || bytes+len(image) > 4<<20 {
			continue
		}
		parsed, err := url.Parse(image)
		if err != nil || !(parsed.Scheme == "https" || parsed.Scheme == "http" || strings.HasPrefix(image, "data:image/")) {
			continue
		}
		seen[image] = true
		unique = append(unique, image)
		bytes += len(image)
	}
	item.ImageURLs = unique
	if item.FileChanges == nil {
		item.FileChanges = []ActivityFileChange{}
	}
}

func compactActivity(value string) string {
	value = strings.Join(strings.Fields(value), " ")
	r := []rune(value)
	if len(r) > 240 {
		return string(r[:239]) + "…"
	}
	return value
}
func findActivityString(value any, keys []string, depth int) string {
	if depth > 4 {
		return ""
	}
	switch v := value.(type) {
	case string:
		var nested any
		if json.Unmarshal([]byte(v), &nested) == nil {
			return findActivityString(nested, keys, depth+1)
		}
		return strings.TrimSpace(v)
	case []any:
		for _, entry := range v {
			if text := findActivityString(entry, keys, depth+1); text != "" {
				return text
			}
		}
	case map[string]any:
		for _, key := range keys {
			if text := activityString(v[key]); strings.TrimSpace(text) != "" {
				return text
			}
			if list, ok := v[key].([]any); ok {
				for _, entry := range list {
					if text := activityString(entry); strings.TrimSpace(text) != "" {
						return text
					}
				}
			}
		}
		for _, key := range []string{"action", "arguments", "args", "input", "params", "rawInput", "raw_input", "toolInput", "tool_input"} {
			if nested := v[key]; nested != nil {
				if text := findActivityString(nested, keys, depth+1); text != "" {
					return text
				}
			}
		}
	}
	return ""
}
func activityTarget(kind string, value any) string {
	var keys []string
	switch kind {
	case KindCommand:
		keys = []string{"command", "cmd"}
	case KindFileRead:
		keys = []string{"filePath", "file_path", "path", "targetFile", "target_file", "notebookPath", "notebook_path"}
	case KindFileSearch:
		keys = []string{"pattern", "query", "regex", "glob"}
	case KindFileList:
		keys = []string{"path", "directory", "dir", "root"}
	case KindSearch:
		keys = []string{"query", "queries", "url"}
	case KindTool:
		keys = []string{"title", "code"}
	default:
		return ""
	}
	return findActivityString(value, keys, 0)
}
func toolCallSummary(value any, depth int) string {
	if depth > 4 {
		return ""
	}
	object := activityObject(value)
	if object == nil {
		return ""
	}
	if calls, ok := object["toolCalls"].([]any); ok {
		names := []string{}
		for _, call := range calls {
			row := activityObject(call)
			name := activityString(row["tool"])
			if name == "" {
				name = activityString(row["name"])
			}
			if name != "" {
				names = append(names, name)
			}
			if len(names) >= 32 {
				break
			}
		}
		if len(names) > 0 {
			return strings.Join(names, ", ")
		}
	}
	for _, key := range []string{"metadata", "state", "result", "content"} {
		if text := toolCallSummary(object[key], depth+1); text != "" {
			return text
		}
	}
	return ""
}
func imageActivity(value any) bool {
	object := activityObject(value)
	kind := activityString(object["type"])
	mime := activityString(object["mime"])
	if mime == "" {
		mime = activityString(object["mimeType"])
	}
	if mime == "" {
		mime = activityString(object["mime_type"])
	}
	return kind == "image" || kind == "inputImage" || (kind == "file" && strings.HasPrefix(mime, "image/"))
}
func activityOutput(value any, depth int) string {
	if depth > 6 || value == nil {
		return ""
	}
	if imageActivity(value) {
		return ""
	}
	switch v := value.(type) {
	case string:
		return v
	case []any:
		parts := []string{}
		for _, entry := range v {
			if text := activityOutput(entry, depth+1); text != "" {
				parts = append(parts, text)
			}
			if len(parts) >= 256 {
				break
			}
		}
		return strings.Join(parts, "\n\n")
	case map[string]any:
		if structured := v["structuredContent"]; structured != nil {
			raw, _ := json.MarshalIndent(structured, "", "  ")
			return string(raw)
		}
		if text := activityString(v["aggregatedOutput"]); text != "" {
			return text
		}
		streams := []string{}
		for _, key := range []string{"stdout", "stderr"} {
			if text := activityString(v[key]); text != "" {
				streams = append(streams, text)
			}
		}
		if len(streams) > 0 {
			return strings.Join(streams, "\n")
		}
		for _, key := range []string{"content", "result", "output", "message", "text"} {
			if nested := v[key]; nested != nil {
				return activityOutput(nested, depth+1)
			}
		}
		// This is an explicit public result value, not the source event or
		// metadata object. Preserve structured tool data as Rust does.
		raw, _ := json.MarshalIndent(v, "", "  ")
		return string(raw)
	default:
		raw, _ := json.Marshal(value)
		return string(raw)
	}
}
func collectActivityImages(value any, images *[]string, depth int) {
	if depth > 6 || len(*images) >= 64 {
		return
	}
	switch v := value.(type) {
	case []any:
		for _, entry := range v {
			collectActivityImages(entry, images, depth+1)
			if len(*images) >= 64 {
				return
			}
		}
	case map[string]any:
		if imageActivity(v) {
			image := ""
			for _, key := range []string{"imageUrl", "image_url", "url", "uri"} {
				if image == "" {
					image = activityString(v[key])
				}
			}
			if image == "" {
				data := activityString(v["data"])
				source := activityObject(v["source"])
				if data == "" {
					data = activityString(source["data"])
				}
				mime := ""
				for _, key := range []string{"mime", "mimeType", "mime_type"} {
					if mime == "" {
						mime = activityString(v[key])
					}
				}
				if mime == "" {
					mime = activityString(source["media_type"])
				}
				if mime == "" {
					mime = "image/png"
				}
				if data != "" && strings.HasPrefix(mime, "image/") {
					image = "data:" + mime + ";base64," + data
				}
			}
			if image != "" {
				*images = append(*images, image)
			}
		}
		for _, key := range []string{"content", "attachments", "files", "result", "state", "output"} {
			collectActivityImages(v[key], images, depth+1)
		}
	}
}

func extractActivityChanges(value any, changes *[]ActivityFileChange, depth int) {
	if depth > 4 || len(*changes) >= 128 {
		return
	}
	switch v := value.(type) {
	case string:
		var nested any
		if json.Unmarshal([]byte(v), &nested) == nil {
			extractActivityChanges(nested, changes, depth+1)
		} else {
			extractActivityPatch(v, changes)
		}
	case []any:
		for _, entry := range v {
			extractActivityChanges(entry, changes, depth+1)
			if len(*changes) >= 128 {
				return
			}
		}
	case map[string]any:
		path := findActivityStringDirect(v, []string{"path", "filePath", "file_path", "filename", "fileName", "targetFile", "target_file", "notebookPath", "notebook_path"})
		if path != "" {
			change := ActivityFileChange{Path: activityLimit(path, 4096)}
			status := activityString(v["type"])
			if status == "" {
				status = activityString(v["kind"])
			}
			if status == "" {
				status = activityString(activityObject(v["kind"])["type"])
			}
			switch status {
			case "add", "create", "added", "write":
				status = "added"
			case "delete", "deleted", "remove":
				status = "deleted"
			case "update", "edit", "modify", "modified":
				status = "modified"
			default:
				status = ""
			}
			if status != "" {
				change.Status = &status
			}
			if diff := findActivityStringDirect(v, []string{"diff", "unifiedDiff", "unified_diff"}); diff != "" {
				diff = limitActivityDiff(diff)
				change.Diff = &diff
				add, del := countActivityDiff(diff)
				change.Additions = &add
				change.Deletions = &del
			}
			if hunks, ok := v["structuredPatch"].([]any); ok && len(hunks) > 0 {
				diff := activityStructuredPatch(hunks)
				change.Diff = &diff
				add, del := countActivityDiff(diff)
				change.Additions = &add
				change.Deletions = &del
			} else if change.Diff == nil {
				old, oldOK := activityFieldString(v, []string{"oldString", "old_string", "oldStr", "old_str", "oldText", "old_text", "oldContent", "old_content", "oldSource", "old_source"})
				next, newOK := activityFieldString(v, []string{"newString", "new_string", "newStr", "new_str", "newText", "new_text", "newContent", "new_content", "newSource", "new_source"})
				diff := ""
				if oldOK && newOK {
					diff = activityReplacement(old, next)
				} else if content, ok := v["content"].(string); ok {
					prefix := "+"
					if status == "deleted" {
						prefix = "-"
					}
					lines := strings.Split(strings.TrimSuffix(content, "\n"), "\n")
					if content != "" {
						for i, line := range lines {
							lines[i] = prefix + line
						}
						diff = limitActivityDiff(strings.Join(lines, "\n"))
					}
				}
				if diff != "" {
					change.Diff = &diff
					add, del := countActivityDiff(diff)
					change.Additions = &add
					change.Deletions = &del
				}
			}
			mergeActivityChange(changes, change)
		}
		for _, key := range []string{"changes", "fileChanges", "file_changes", "arguments", "args", "input", "rawInput", "raw_input", "toolInput", "tool_input", "metadata", "state"} {
			nested := v[key]
			if key == "changes" || key == "fileChanges" || key == "file_changes" {
				if rows, ok := nested.(map[string]any); ok {
					paths := make([]string, 0, len(rows))
					for path := range rows {
						paths = append(paths, path)
					}
					sort.Strings(paths)
					for _, path := range paths {
						row := rows[path]
						if len(*changes) >= 128 {
							break
						}
						if entry := activityObject(row); entry != nil {
							copy := map[string]any{}
							for k, x := range entry {
								copy[k] = x
							}
							if findActivityStringDirect(copy, []string{"path", "filePath", "file_path"}) == "" {
								copy["path"] = path
							}
							extractActivityChanges(copy, changes, depth+1)
						}
					}
					continue
				}
			}
			extractActivityChanges(nested, changes, depth+1)
		}
		for _, key := range []string{"patch", "patchText", "patch_text"} {
			extractActivityPatch(activityString(v[key]), changes)
		}
	}
}
func findActivityStringDirect(object map[string]any, keys []string) string {
	for _, key := range keys {
		if text := activityString(object[key]); strings.TrimSpace(text) != "" {
			return text
		}
	}
	return ""
}
func mergeActivityChange(changes *[]ActivityFileChange, change ActivityFileChange) {
	for i, old := range *changes {
		if old.Path == change.Path {
			if change.Diff != nil {
				old.Diff = change.Diff
				old.Additions = change.Additions
				old.Deletions = change.Deletions
			}
			if change.Status != nil {
				old.Status = change.Status
			}
			(*changes)[i] = old
			return
		}
	}
	if len(*changes) < 128 {
		*changes = append(*changes, change)
	}
}
func limitActivityDiff(diff string) string {
	lines := strings.Split(diff, "\n")
	if len(lines) > 1000 {
		lines = lines[:1000]
		lines = append(lines, "[truncated]")
	}
	return activityLimit(strings.Join(lines, "\n"), activityTextLimit)
}
func countActivityDiff(diff string) (uint64, uint64) {
	var add, del uint64
	for _, line := range strings.Split(diff, "\n") {
		if strings.HasPrefix(line, "+") && !strings.HasPrefix(line, "+++") {
			add++
		}
		if strings.HasPrefix(line, "-") && !strings.HasPrefix(line, "---") {
			del++
		}
	}
	return add, del
}
func extractActivityPatch(patch string, changes *[]ActivityFileChange) {
	lines := strings.Split(activityLimit(patch, 64000), "\n")
	var current *ActivityFileChange
	diff := []string{}
	oldPath := ""
	flush := func() {
		if current == nil {
			return
		}
		text := limitActivityDiff(strings.Join(diff, "\n"))
		current.Diff = &text
		add, del := countActivityDiff(text)
		current.Additions = &add
		current.Deletions = &del
		mergeActivityChange(changes, *current)
	}
	for _, line := range lines {
		if strings.HasPrefix(line, "diff --git ") {
			flush()
			current = nil
			diff = nil
			oldPath = ""
			parts := strings.Fields(strings.TrimPrefix(line, "diff --git "))
			if len(parts) == 2 {
				status := "modified"
				current = &ActivityFileChange{Path: strings.TrimPrefix(parts[1], "b/"), Status: &status}
			}
			continue
		}
		if strings.HasPrefix(line, "new file mode ") && current != nil {
			status := "added"
			current.Status = &status
			continue
		}
		if strings.HasPrefix(line, "deleted file mode ") && current != nil {
			status := "deleted"
			current.Status = &status
			continue
		}
		if strings.HasPrefix(line, "--- ") {
			oldPath = strings.TrimPrefix(strings.TrimPrefix(line, "--- "), "a/")
			continue
		}
		path, status := "", "modified"
		for _, prefix := range []string{"*** Update File: ", "*** Add File: ", "*** Delete File: "} {
			if strings.HasPrefix(line, prefix) {
				path = strings.TrimPrefix(line, prefix)
				if strings.Contains(prefix, "Add") {
					status = "added"
				}
				if strings.Contains(prefix, "Delete") {
					status = "deleted"
				}
			}
		}
		if strings.HasPrefix(line, "+++ ") {
			path = strings.TrimPrefix(strings.TrimPrefix(line, "+++ "), "b/")
			if path == "/dev/null" {
				path = oldPath
				status = "deleted"
			} else if oldPath == "/dev/null" {
				status = "added"
			}
		}
		if path != "" {
			if current != nil && current.Path == strings.TrimSpace(path) {
				if status != "modified" {
					current.Status = &status
				}
				continue
			}
			flush()
			current = &ActivityFileChange{Path: strings.TrimSpace(path), Status: &status}
			diff = nil
			continue
		}
		if current != nil {
			diff = append(diff, line)
		}
	}
	flush()
}

func activityFieldString(value map[string]any, keys []string) (string, bool) {
	for _, key := range keys {
		if text, ok := value[key].(string); ok {
			return text, true
		}
	}
	return "", false
}
func activityReplacement(old, next string) string {
	before, after := strings.Split(strings.TrimSuffix(old, "\n"), "\n"), strings.Split(strings.TrimSuffix(next, "\n"), "\n")
	if old == "" {
		before = nil
	}
	if next == "" {
		after = nil
	}
	first := 0
	for first < len(before) && first < len(after) && before[first] == after[first] {
		first++
	}
	endOld, endNew := len(before), len(after)
	for endOld > first && endNew > first && before[endOld-1] == after[endNew-1] {
		endOld--
		endNew--
	}
	if first == endOld && first == endNew {
		return ""
	}
	lines := []string{fmt.Sprintf("@@ -%d,%d +%d,%d @@", first+1, endOld-first, first+1, endNew-first)}
	for _, line := range before[first:endOld] {
		lines = append(lines, "-"+line)
	}
	for _, line := range after[first:endNew] {
		lines = append(lines, "+"+line)
	}
	return limitActivityDiff(strings.Join(lines, "\n"))
}
func activityStructuredPatch(hunks []any) string {
	lines := []string{}
	number := func(value map[string]any, keys ...string) int {
		for _, key := range keys {
			if n, ok := value[key].(float64); ok && n >= 0 {
				return int(n)
			}
		}
		return 1
	}
	for _, raw := range hunks {
		h := activityObject(raw)
		rows, _ := h["lines"].([]any)
		if len(rows) == 0 {
			continue
		}
		lines = append(lines, fmt.Sprintf("@@ -%d,%d +%d,%d @@", number(h, "oldStart", "old_start"), number(h, "oldLines", "old_lines"), number(h, "newStart", "new_start"), number(h, "newLines", "new_lines")))
		for _, line := range rows {
			if text, ok := line.(string); ok {
				lines = append(lines, text)
			}
			if len(lines) > 1000 {
				break
			}
		}
		if len(lines) > 1000 {
			break
		}
	}
	return limitActivityDiff(strings.Join(lines, "\n"))
}
