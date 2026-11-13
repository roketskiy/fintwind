package model

import "strings"

// ToolKind follows the protocol's exact tool-name classification. Only known
// complete leaf names match; arbitrary MCP names are not file operations.
func ToolKind(name string) string {
	normalized := []byte(strings.TrimSpace(name))
	for i, b := range normalized {
		if b >= 'A' && b <= 'Z' {
			normalized[i] = b + ('a' - 'A')
		} else if b == '-' || b == ' ' {
			normalized[i] = '_'
		}
	}
	leaf := string(normalized)
	if i := strings.LastIndex(leaf, "__"); i >= 0 {
		leaf = leaf[i+2:]
	}
	if i := strings.LastIndexAny(leaf, ":./"); i >= 0 {
		leaf = leaf[i+1:]
	}
	compact := strings.ReplaceAll(leaf, "_", "")
	switch compact {
	case "todo", "todowrite", "updateplan", "plan":
		return KindPlan
	case "bash", "command", "executecommand", "commandexecution", "runcommand", "runterminalcommand", "shell", "shellcommand", "terminal":
		return KindCommand
	case "applypatch", "create", "createfile", "delete", "deletefile", "edit", "filechange", "fileedit", "editfile", "move", "movefile", "multiedit", "notebookedit", "patch", "rename", "renamefile", "replace", "savefile", "strreplace", "write", "writefile":
		return KindFileChange
	case "read", "fileread", "readfile", "readtextfile", "viewfile":
		return KindFileRead
	case "filesearch", "find", "findfiles", "glob", "grep", "ripgrep", "searchfiles", "searchinfiles":
		return KindFileSearch
	case "directorylist", "filelist", "list", "listdirectory", "listfiles", "ls", "readdir":
		return KindFileList
	case "search", "searchtool", "webfetch", "websearch":
		return KindSearch
	default:
		return KindTool
	}
}
