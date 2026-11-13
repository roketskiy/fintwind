// Package composerpaste adds Markdown clipboard handling to the native composer.
package composerpaste

import (
	"strings"
	"unicode/utf8"

	"github.com/JohannesKaufmann/html-to-markdown/v2/converter"
	"github.com/JohannesKaufmann/html-to-markdown/v2/plugin/base"
	"github.com/JohannesKaufmann/html-to-markdown/v2/plugin/commonmark"
	"github.com/JohannesKaufmann/html-to-markdown/v2/plugin/table"
	"github.com/egoist/mygo/transfer"
	"github.com/egoist/mygo/ui"
	"golang.org/x/net/html"
)

type paste struct{ plain, html, before string }
type replacement struct {
	expected, text string
	caret          int
	before         string
}

// State belongs to one composer. All its mutations run on the UI thread.
type State struct {
	target     string
	generation uint64
	plainOnly  bool
	pending    *paste
	result     *replacement
	inflight   *replacement
	previous   string
	edits      []replacement
}

// Prepare runs before constructing the input, so its external text update is
// folded into the native paste's undo step rather than a separate edit.
func (s *State) Prepare(target string, draft *string) (caret int, changed bool) {
	if s.target != target {
		s.target = target
		s.generation++
		s.pending, s.result, s.inflight = nil, nil, nil
		s.plainOnly = false
		s.edits = nil
		s.previous = *draft
	}
	defer func() { s.previous = *draft }()
	if r := s.result; r != nil {
		s.result = nil
		if *draft == r.expected {
			*draft = r.text
			edit := *r
			edit.expected = ""
			s.edits = append(s.edits, edit)
			if len(s.edits) > 200 {
				s.edits[0] = replacement{}
				s.edits = s.edits[1:]
			}
			return r.caret, true
		}
	}
	// The pinned editor merges reformatting into paste history, but records
	// its redo caret before SetTextSelection. Restore it on the corresponding
	// undo-to-redo transition, including the native context menu's Redo.
	if s.previous != *draft {
		for i := len(s.edits) - 1; i >= 0; i-- {
			r := &s.edits[i]
			if s.previous == r.before && *draft == r.text {
				return r.caret, true
			}
		}
	}
	return 0, false
}

// TextKey leaves insertion to the native editor, including its selection and
// undo handling. Ctrl+Shift+V bypasses rich content for this paste only.
func (s *State) TextKey(k ui.TextKey) {
	if s.inflight != nil {
		s.generation++
		s.inflight, s.result = nil, nil
	}
	if k.Key == ui.KeyV && k.Mods&(ui.Ctrl|ui.Alt|ui.Super) == ui.Ctrl {
		s.plainOnly = k.Mods&ui.Shift != 0
	}
}

// Bind observes the completed native paste and converts its HTML off-thread.
// post must dispatch the callback onto the owning window's UI thread.
func (s *State) Bind(input ui.Element, draft *string, services ui.Services, post func(func())) {
	start, end := input.TextSelection()
	if r := s.inflight; r != nil && (*draft != r.expected || start != end || end != r.caret) {
		s.generation++
		s.inflight, s.result = nil, nil
	}
	if p := s.pending; p != nil {
		s.pending = nil
		start, end := input.TextSelection()
		runes := []rune(*draft)
		from := end - utf8.RuneCountInString(p.plain)
		if start == end && from >= 0 && end <= len(runes) && string(runes[from:end]) == p.plain {
			expected, prefix, suffix := *draft, string(runes[:from]), string(runes[end:])
			generation := s.generation
			s.inflight = &replacement{expected: expected, caret: end}
			go func() {
				conv := converter.NewConverter(converter.WithPlugins(
					base.NewBasePlugin(), commonmark.NewCommonmarkPlugin(),
					table.NewTablePlugin(table.WithNewlineBehavior(table.NewlineBehaviorPreserve)),
				))
				markdown, err := conv.ConvertString(p.html)
				// Code editors commonly advertise syntax-colored div/span HTML
				// with CSS preformatted whitespace, without semantic code tags.
				// Their plain representation is the authoritative source text.
				if preformattedEditorHTML(p.html) {
					markdown, err = p.plain, nil
				}
				valid := err == nil && strings.TrimSpace(markdown) != ""
				post(func() {
					if s.generation == generation {
						s.inflight = nil
						if valid && *draft == expected && markdown != p.plain {
							s.result = &replacement{expected: expected, text: prefix + markdown + suffix,
								caret: from + utf8.RuneCountInString(markdown), before: p.before}
						}
					}
				})
			}()
		}
	}
	input.OnPaste(func(text string) bool {
		s.generation++
		s.pending, s.result, s.inflight = nil, nil, nil
		plainOnly := s.plainOnly
		s.plainOnly = false
		if plainOnly || text == "" {
			return false
		}
		data := services.ReadClipboardData(transfer.HTML)
		html, err := data.Read(transfer.HTML)
		// Large pastes remain plain text, avoiding a second large document in
		// memory. Never alter or replace the user's clipboard representations.
		if err == nil && len(html) > 0 && len(html) <= 2<<20 {
			s.pending = &paste{strings.ReplaceAll(text, "\r\n", "\n"), string(html), *draft}
		}
		return false
	})
}

func preformattedEditorHTML(fragment string) bool {
	if !strings.Contains(strings.ToLower(fragment), "white-space") {
		return false
	}
	doc, err := html.Parse(strings.NewReader(fragment))
	if err != nil {
		return false
	}
	stack := []*html.Node{doc}
	for len(stack) > 0 {
		n := stack[len(stack)-1]
		stack = stack[:len(stack)-1]
		if n.Type == html.ElementNode && n.Data == "div" {
			for _, attr := range n.Attr {
				if attr.Key != "style" {
					continue
				}
				for _, declaration := range strings.Split(attr.Val, ";") {
					name, value, _ := strings.Cut(declaration, ":")
					if strings.EqualFold(strings.TrimSpace(name), "white-space") && strings.EqualFold(strings.TrimSpace(value), "pre") {
						return true
					}
				}
			}
		}
		for child := n.FirstChild; child != nil; child = child.NextSibling {
			stack = append(stack, child)
		}
	}
	return false
}
