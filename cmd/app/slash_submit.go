package main

import (
	"strings"
	"unicode"
)

func expandSlashSubmission(text string, commands []slashCommand) string {
	invocation, ok := strings.CutPrefix(text, "/")
	if !ok {
		return text
	}
	index := strings.IndexFunc(invocation, unicode.IsSpace)
	name, args := invocation, ""
	if index >= 0 {
		name, args = invocation[:index], strings.TrimSpace(invocation[index:])
	}
	for _, command := range commands {
		if command.name != name || command.template == nil {
			continue
		}
		template := *command.template
		words := strings.Fields(args)
		var out strings.Builder
		consumed := false
		for len(template) > 0 {
			i := strings.IndexByte(template, '$')
			if i < 0 {
				out.WriteString(template)
				break
			}
			out.WriteString(template[:i])
			template = template[i+1:]
			switch {
			case strings.HasPrefix(template, "ARGUMENTS"):
				out.WriteString(args)
				template = template[len("ARGUMENTS"):]
				consumed = true
			case strings.HasPrefix(template, "@"):
				out.WriteString(args)
				template = template[1:]
				consumed = true
			case len(template) > 0 && template[0] >= '1' && template[0] <= '9':
				index := int(template[0] - '1')
				if index < len(words) {
					out.WriteString(words[index])
				}
				template = template[1:]
				consumed = true
			default:
				out.WriteByte('$')
			}
		}
		if !consumed && args != "" {
			out.WriteString("\n\n")
			out.WriteString(args)
		}
		return out.String()
	}
	return text
}
