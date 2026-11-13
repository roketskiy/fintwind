package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// The OpenCode global configuration is a JSONC document the desktop edits in
// place — the client side of Rust's opencode_shell, opencode_todo, and
// opencode_image_generate helpers. JSONC wins over JSON, comments and
// unrelated formatting survive every write, and a missing file starts from
// OpenCode's template.

const opencodeConfigTemplate = "{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n"

// opencodeConfigPath mirrors opencode_shell::config_path: OPENCODE_CONFIG_DIR
// wins, then XDG_CONFIG_HOME/opencode, then ~/.config/opencode; the JSONC
// file wins over the JSON one.
func opencodeConfigPath() string {
	directory := ""
	if value := os.Getenv("OPENCODE_CONFIG_DIR"); value != "" {
		directory = value
	} else if value := os.Getenv("XDG_CONFIG_HOME"); value != "" {
		directory = filepath.Join(value, "opencode")
	} else {
		home, err := os.UserHomeDir()
		if err != nil {
			home = os.TempDir()
		}
		directory = filepath.Join(home, ".config", "opencode")
	}
	jsonc := filepath.Join(directory, "opencode.jsonc")
	if _, err := os.Stat(jsonc); err == nil {
		return jsonc
	}
	return filepath.Join(directory, "opencode.json")
}

func opencodeReadDocument(path string) (string, error) {
	text, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return opencodeConfigTemplate, nil
		}
		return "", err
	}
	return string(text), nil
}

// opencodeWriteDocument replaces the file atomically: a temp sibling then a
// rename, like the Rust helpers.
func opencodeWriteDocument(path, text string) error {
	if parent := filepath.Dir(path); parent != "" {
		if err := os.MkdirAll(parent, 0o755); err != nil {
			return err
		}
	}
	temporary := path + fmt.Sprintf(".%d.tmp", os.Getpid())
	if err := os.WriteFile(temporary, []byte(text), 0o644); err != nil {
		return err
	}
	if err := os.Rename(temporary, path); err != nil {
		os.Remove(temporary)
		return err
	}
	return nil
}

// opencodeLoadShell reads the shell preference, falling back to the JSON
// sibling when the JSONC file does not select one.
func opencodeLoadShell() (string, error) {
	path := opencodeConfigPath()
	shell, err := opencodeLoadShellAt(path)
	if err != nil {
		return "", err
	}
	if shell == "" && filepath.Ext(path) == ".jsonc" {
		return opencodeLoadShellAt(path[:len(path)-len(".jsonc")] + ".json")
	}
	return shell, nil
}

func opencodeLoadShellAt(path string) (string, error) {
	text, err := opencodeReadDocument(path)
	if err != nil {
		return "", err
	}
	value, err := jsoncGetProperty(text, nil, "shell")
	if err != nil {
		return "", err
	}
	if value == "" {
		return "", nil
	}
	var shell string
	if err := json.Unmarshal([]byte(value), &shell); err != nil {
		return "", nil
	}
	return shell, nil
}

// opencodeSaveShell writes the shell choice; an empty shell removes the key.
// Clearing the JSONC also clears a JSON sibling, like opencode_shell::save.
func opencodeSaveShell(shell string) error {
	path := opencodeConfigPath()
	value := ""
	if shell != "" {
		encoded, err := json.Marshal(shell)
		if err != nil {
			return err
		}
		value = string(encoded)
	}
	if shell == "" && filepath.Ext(path) == ".jsonc" {
		sibling := path[:len(path)-len(".jsonc")] + ".json"
		if _, err := os.Stat(sibling); err == nil {
			if err := opencodeWriteKey(sibling, "", "shell", ""); err != nil {
				return err
			}
		}
	}
	return opencodeWriteKey(path, "", "shell", value)
}

func opencodeLoadTodo() (bool, error) {
	text, err := opencodeReadDocument(opencodeConfigPath())
	if err != nil {
		return false, err
	}
	value, err := jsoncGetProperty(text, []string{"experimental"}, "todo")
	if err != nil {
		return false, err
	}
	return value == "true", nil
}

func opencodeSaveTodo(enabled bool) error {
	return opencodeWriteKey(opencodeConfigPath(), "experimental", "todo", choice(enabled, "true", "false"))
}

// opencodeLoadImageGenerate reads the experimental.image_generate entry,
// which accepts true or an object with a default model.
func opencodeLoadImageGenerate() (enabled bool, model string, err error) {
	text, readErr := opencodeReadDocument(opencodeConfigPath())
	if readErr != nil {
		return false, "", readErr
	}
	value, err := jsoncGetProperty(text, []string{"experimental"}, "image_generate")
	if err != nil || value == "" {
		return false, "", err
	}
	var parsed any
	if json.Unmarshal([]byte(value), &parsed) != nil {
		return false, "", nil
	}
	switch entry := parsed.(type) {
	case bool:
		return entry, "", nil
	case map[string]any:
		model, _ := entry["model"].(string)
		return true, model, nil
	}
	return false, "", nil
}

// opencodeSaveImageGenerate writes the switch: enabled with a model stores
// the object form, everything else stores the plain boolean.
func opencodeSaveImageGenerate(enabled bool, model string) error {
	value := choice(enabled, "true", "false")
	if enabled && model != "" {
		encoded, err := json.Marshal(map[string]string{"model": model})
		if err != nil {
			return err
		}
		value = string(encoded)
	}
	return opencodeWriteKey(opencodeConfigPath(), "experimental", "image_generate", value)
}

// opencodeWriteKey edits the document at path so parent.name equals the raw
// JSON value; an empty value removes the key. parent is empty for a
// top-level key.
func opencodeWriteKey(path, parent, name, value string) error {
	text, err := opencodeReadDocument(path)
	if err != nil {
		return err
	}
	if parent == "" {
		text, err = jsoncSetProperty(text, "top", name, value)
	} else {
		text, err = jsoncSetProperty(text, parent, name, value)
	}
	if err != nil {
		return err
	}
	return opencodeWriteDocument(path, text)
}

// jsoncGetProperty returns the raw JSON text of one property, or "" when it
// is absent. parent lists the objects to descend through, in order.
func jsoncGetProperty(text string, parent []string, name string) (string, error) {
	open, err := jsoncRootOpen(text)
	if err != nil {
		return "", err
	}
	for _, key := range parent {
		props, _, err := jsoncObjectProperties(text, open)
		if err != nil {
			return "", err
		}
		prop := jsoncFindProp(props, key)
		if prop == nil {
			return "", nil
		}
		if open, err = jsoncObjectOpen(text, prop.valueStart); err != nil {
			return "", err
		}
	}
	props, _, err := jsoncObjectProperties(text, open)
	if err != nil {
		return "", err
	}
	prop := jsoncFindProp(props, name)
	if prop == nil {
		return "", nil
	}
	return text[prop.valueStart:prop.valueEnd], nil
}

func jsoncRootOpen(text string) (int, error) {
	i := jsoncSkipSpace(text, 0)
	if i >= len(text) || text[i] != '{' {
		return 0, errors.New("OpenCode config must be an object")
	}
	return i, nil
}

// jsoncObjectOpen returns the '{' index when the value at start is an
// object, or an error when it is any other value.
func jsoncObjectOpen(text string, valueStart int) (int, error) {
	i := jsoncSkipSpace(text, valueStart)
	if i < len(text) && text[i] == '{' {
		return i, nil
	}
	return 0, errors.New("OpenCode config value must be an object")
}

// jsoncProperty is one object property's layout in the document text.
type jsoncProperty struct {
	name       string
	keyStart   int
	valueStart int
	valueEnd   int // one past the value's last byte
}

func jsoncFindProp(props []jsoncProperty, name string) *jsoncProperty {
	for i := range props {
		if props[i].name == name {
			return &props[i]
		}
	}
	return nil
}

// jsoncSkipSpace advances past whitespace and comments.
func jsoncSkipSpace(text string, i int) int {
	for i < len(text) {
		switch c := text[i]; {
		case c == ' ' || c == '\t' || c == '\r' || c == '\n':
			i++
		case c == '/' && i+1 < len(text) && text[i+1] == '/':
			i += 2
			for i < len(text) && text[i] != '\n' {
				i++
			}
		case c == '/' && i+1 < len(text) && text[i+1] == '*':
			i += 2
			for i+1 < len(text) && !(text[i] == '*' && text[i+1] == '/') {
				i++
			}
			if i+1 < len(text) {
				i += 2
			} else {
				return len(text)
			}
		default:
			return i
		}
	}
	return i
}

// jsoncScanString returns the index one past the closing quote of the string
// that opens at open.
func jsoncScanString(text string, open int) (int, error) {
	i := open + 1
	for i < len(text) {
		switch text[i] {
		case '\\':
			i += 2
		case '"':
			return i + 1, nil
		default:
			i++
		}
	}
	return 0, errors.New("unterminated string in OpenCode config")
}

// jsoncScanValue returns the index one past the value that starts at start.
// Composite values end where their nesting closes; bare literals end at the
// first structural boundary. Strings and comments are skipped throughout.
func jsoncScanValue(text string, start int) (int, error) {
	i := jsoncSkipSpace(text, start)
	if i >= len(text) {
		return 0, errors.New("unterminated value in OpenCode config")
	}
	if text[i] == '"' {
		return jsoncScanString(text, i)
	}
	if text[i] != '{' && text[i] != '[' {
		for i < len(text) {
			c := text[i]
			if c == ',' || c == '}' || c == ']' || c == ' ' || c == '\t' || c == '\r' || c == '\n' {
				return i, nil
			}
			if c == '/' && i+1 < len(text) && (text[i+1] == '/' || text[i+1] == '*') {
				return i, nil
			}
			i++
		}
		return 0, errors.New("unterminated value in OpenCode config")
	}
	depth := 0
	for i < len(text) {
		switch text[i] {
		case '"':
			end, err := jsoncScanString(text, i)
			if err != nil {
				return 0, err
			}
			i = end
			continue
		case '{', '[':
			depth++
		case '}', ']':
			depth--
			if depth == 0 {
				return i + 1, nil
			}
		case '/':
			if i+1 < len(text) && (text[i+1] == '/' || text[i+1] == '*') {
				i = jsoncSkipSpace(text, i)
				continue
			}
		}
		i++
	}
	return 0, errors.New("unterminated value in OpenCode config")
}

// jsoncObjectProperties parses the properties of the object whose '{' is at
// open, in order, with the index of its '}'.
func jsoncObjectProperties(text string, open int) ([]jsoncProperty, int, error) {
	props := []jsoncProperty{}
	i := open + 1
	for {
		i = jsoncSkipSpace(text, i)
		if i >= len(text) {
			return nil, 0, errors.New("unterminated object in OpenCode config")
		}
		if text[i] == '}' {
			return props, i, nil
		}
		if text[i] != '"' {
			return nil, 0, errors.New("expected a property name in OpenCode config")
		}
		keyEnd, err := jsoncScanString(text, i)
		if err != nil {
			return nil, 0, err
		}
		var name string
		if err := json.Unmarshal([]byte(text[i:keyEnd]), &name); err != nil {
			return nil, 0, errors.New("invalid property name in OpenCode config")
		}
		colon := jsoncSkipSpace(text, keyEnd)
		if colon >= len(text) || text[colon] != ':' {
			return nil, 0, errors.New("expected ':' in OpenCode config")
		}
		valueStart := jsoncSkipSpace(text, colon+1)
		valueEnd, err := jsoncScanValue(text, valueStart)
		if err != nil {
			return nil, 0, err
		}
		props = append(props, jsoncProperty{name: name, keyStart: i, valueStart: valueStart, valueEnd: valueEnd})
		i = jsoncSkipSpace(text, valueEnd)
		if i < len(text) && text[i] == ',' {
			i++
		}
	}
}

// jsoncRemoveProperty cuts the first property with the name and one adjacent
// comma, leaving everything else byte-identical. changed reports whether the
// document contained the name at all.
func jsoncRemoveProperty(text string, open int, name string) (string, bool, error) {
	props, _, err := jsoncObjectProperties(text, open)
	if err != nil {
		return "", false, err
	}
	index := -1
	for i, prop := range props {
		if prop.name == name {
			index = i
			break
		}
	}
	if index < 0 {
		return text, false, nil
	}
	prop := props[index]
	start, end := prop.keyStart, prop.valueEnd
	if after := jsoncSkipSpace(text, end); after < len(text) && text[after] == ',' {
		end = after + 1
	} else if index > 0 {
		// The last property: drop the comma that separates it from the
		// previous one, so the object stays valid JSONC.
		before := jsoncSkipSpace(text, props[index-1].valueEnd)
		if before < len(text) && text[before] == ',' {
			start = before
		}
	}
	return text[:start] + text[end:], true, nil
}

// jsoncSetProperty sets one object property: every existing property with
// the name goes away and a fresh one is appended before the closing brace,
// mirroring the Rust CST editor's remove-all-then-append discipline. An
// empty value removes the property. parent is "top" for the root object or
// the name of the object property to edit, such as "experimental".
func jsoncSetProperty(text, parent, name, value string) (string, error) {
	root, err := jsoncRootOpen(text)
	if err != nil {
		return "", err
	}
	open := root
	indent := 1
	if parent != "top" {
		props, _, err := jsoncObjectProperties(text, root)
		if err != nil {
			return "", err
		}
		prop := jsoncFindProp(props, parent)
		if prop != nil {
			if open, err = jsoncObjectOpen(text, prop.valueStart); err != nil {
				// The parent holds a non-object; replace it with an empty
				// object and continue inside.
				if value == "" {
					return text, nil
				}
				text, _, err = jsoncRemoveProperty(text, root, parent)
				if err != nil {
					return "", err
				}
				text, err = jsoncAppendProperty(text, root, parent, "{}", 1)
				if err != nil {
					return "", err
				}
				props, _, err = jsoncObjectProperties(text, root)
				if err != nil {
					return "", err
				}
				prop = jsoncFindProp(props, parent)
				if open, err = jsoncObjectOpen(text, prop.valueStart); err != nil {
					return "", err
				}
			}
		} else {
			if value == "" {
				return text, nil
			}
			text, err = jsoncAppendProperty(text, root, parent, "{}", 1)
			if err != nil {
				return "", err
			}
			props, _, err = jsoncObjectProperties(text, root)
			if err != nil {
				return "", err
			}
			prop = jsoncFindProp(props, parent)
			if open, err = jsoncObjectOpen(text, prop.valueStart); err != nil {
				return "", err
			}
		}
		indent = 2
	}
	if value == "" {
		for {
			var changed bool
			text, changed, err = jsoncRemoveProperty(text, open, name)
			if err != nil {
				return "", err
			}
			if !changed {
				return text, nil
			}
		}
	}
	return jsoncAppendProperty(text, open, name, value, indent)
}

// jsoncAppendProperty cuts every duplicate of the name, then inserts one
// fresh property before the object's closing brace.
func jsoncAppendProperty(text string, open int, name, value string, indent int) (string, error) {
	for {
		var changed bool
		var err error
		text, changed, err = jsoncRemoveProperty(text, open, name)
		if err != nil {
			return "", err
		}
		if !changed {
			break
		}
	}
	_, closeIdx, err := jsoncObjectProperties(text, open)
	if err != nil {
		return "", err
	}
	end := jsoncSkipSpaceBack(text, closeIdx)
	entry := strings.Repeat("  ", indent) + fmt.Sprintf("%q: %s", name, value)
	var inserted string
	switch {
	case end <= open || text[end-1] == '{':
		// The object is empty: the entry opens its body.
		inserted = "\n" + entry + "\n" + strings.Repeat("  ", indent-1)
	case text[end-1] == ',':
		inserted = "\n" + entry
	default:
		inserted = ",\n" + entry
	}
	return text[:end] + inserted + text[closeIdx:], nil
}

// jsoncSkipSpaceBack walks backwards over whitespace and block comments to
// the index one past the last significant byte before close.
func jsoncSkipSpaceBack(text string, close int) int {
	i := close
	for i > 0 {
		c := text[i-1]
		if c == ' ' || c == '\t' || c == '\r' || c == '\n' {
			i--
			continue
		}
		if i >= 2 && text[i-2] == '*' && text[i-1] == '/' {
			// Block comment: find its opener.
			depth := strings.LastIndex(text[:i-2], "/*")
			if depth >= 0 {
				i = depth
				continue
			}
		}
		return i
	}
	return i
}
