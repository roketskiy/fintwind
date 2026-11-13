// pastecheck drives the composer's native input and clipboard without a window,
// user data, subprocesses or provider requests.
package main

import (
	"fmt"
	"os"
	"strings"
	"time"

	"fintwind-go/internal/composerpaste"

	"github.com/egoist/mygo/transfer"
	"github.com/egoist/mygo/ui"
)

type harness struct {
	draft, session string
	state          composerpaste.State
	posts          chan func()
	tester         *ui.Tester
}

func newHarness() *harness {
	h := &harness{session: "first", posts: make(chan func(), 16)}
	h.tester = ui.NewTester(func(c *ui.Context) {
		caret, changed := h.state.Prepare(h.session, &h.draft)
		input := ui.TextAreaBase(c.Key("draft"), &h.draft).Width(600).Height(240).Focus()
		if changed {
			input.SetTextSelection(caret, caret)
		}
		h.state.Bind(input, &h.draft, c.Services(), func(fn func()) { h.posts <- fn })
		input.OnTextKey(func(k ui.TextKey) bool { h.state.TextKey(k); return false })
	}, 640, 300)
	return h
}

func (h *harness) paste(plain, html string, mods ui.Modifiers) {
	h.tester.SetClipboardData(transfer.New(transfer.NewItem(
		transfer.Bytes(transfer.Text, []byte(plain)), transfer.Bytes(transfer.HTML, []byte(html)),
	)))
	h.tester.Key(mods, ui.KeyV)
}

func (h *harness) converted() {
	select {
	case fn := <-h.posts:
		fn()
		h.tester.Frame()
	case <-time.After(5 * time.Second):
		fail("Markdown conversion did not finish")
	}
}

func fail(message string) { fmt.Fprintln(os.Stderr, message); os.Exit(1) }
func require(ok bool, message string) {
	if !ok {
		fail(message)
	}
}

func main() {
	h := newHarness()
	h.paste("Title\nBold link\nFirst\nSecond", `<h2>Title</h2><p><strong>Bold</strong> <a href="https://example.com">link</a></p><ul><li>First</li><li>Second</li></ul>`, ui.Ctrl)
	h.converted()
	for _, fragment := range []string{"## Title", "**Bold**", "[link](https://example.com)", "- First", "- Second"} {
		require(strings.Contains(h.draft, fragment), "missing rich content: "+fragment+" in "+h.draft)
	}
	h.tester.Key(ui.Ctrl, ui.KeyZ)
	require(h.draft == "", "one undo must remove the complete rich paste")
	h.tester.Key(ui.Ctrl, ui.KeyY)
	require(strings.Contains(h.draft, "**Bold**"), "redo must restore Markdown")
	redoText := h.draft
	h.tester.Type("!")
	require(h.draft == redoText+"!", "redo did not restore the paste caret: "+h.draft)
	fmt.Println("PASS rich clipboard, undo and redo")

	h = newHarness()
	h.paste("Name Value\nOne Two", `<table><tr><th>Name</th><th>Value</th></tr><tr><td>One</td><td>Two</td></tr></table>`, ui.Ctrl)
	h.converted()
	require(strings.Contains(h.draft, "| Name") && strings.Contains(h.draft, "| One"), "table was not preserved: "+h.draft)
	fmt.Println("PASS table")

	h = newHarness()
	h.paste("if ready:\n\trun()\n", "<pre><code>if ready:\n\trun()\n</code></pre>", ui.Ctrl)
	h.converted()
	require(strings.Contains(h.draft, "```\nif ready:\n\trun()\n```"), "code whitespace was lost: "+h.draft)
	fmt.Println("PASS code block and tab")

	h = newHarness()
	h.paste("func run() {\n\tcall()\n}\n", `<div style="font-family: Consolas; white-space: pre;"><div><span>func run() {</span></div><div><span>    call()</span></div><div>}</div></div>`, ui.Ctrl)
	h.converted()
	require(h.draft == "func run() {\n\tcall()\n}\n", "syntax-colored editor HTML damaged original code: "+h.draft)
	fmt.Println("PASS code editor HTML")

	h = newHarness()
	plain := "  first\r\n\tsecond\r\n\r\nlast  "
	h.tester.SetClipboard(plain)
	h.tester.Key(ui.Ctrl, ui.KeyV)
	require(h.draft == strings.ReplaceAll(plain, "\r\n", "\n"), "plain whitespace was modified")
	h.tester.Key(ui.Ctrl, ui.KeyA)
	h.paste("Plain\n\ttext", "<p><strong>Plain</strong></p><p>text</p>", ui.Ctrl|ui.Shift)
	require(h.draft == "Plain\n\ttext", "plain paste shortcut converted HTML")
	fmt.Println("PASS plain text and Ctrl+Shift+V")

	h = newHarness()
	h.tester.Type("甲乙丙")
	h.tester.Key(0, ui.KeyLeft)
	h.tester.Key(ui.Shift, ui.KeyLeft)
	h.paste("中文", "<strong>中文</strong>", ui.Ctrl)
	h.converted()
	require(h.draft == "甲**中文**丙", "Unicode selection replacement failed: "+h.draft)
	h.tester.Type("!")
	require(h.draft == "甲**中文**!丙", "caret did not follow formatted paste: "+h.draft)
	h.tester.Key(ui.Ctrl, ui.KeyZ)
	h.tester.Key(ui.Ctrl, ui.KeyZ)
	require(h.draft == "甲乙丙", "undo did not restore selection's original text: "+h.draft)
	h.tester.Key(ui.Ctrl, ui.KeyY)
	h.tester.Type("!")
	require(h.draft == "甲**中文**!丙", "redo did not restore the selection paste caret: "+h.draft)
	fmt.Println("PASS Unicode selection, caret and independent undo")

	h = newHarness()
	h.paste("Bold", "<strong>Bold</strong>", ui.Ctrl)
	h.tester.Type(" edited")
	h.converted()
	require(h.draft == "Bold edited", "late conversion overwrote new input")
	h = newHarness()
	h.paste("Bold", "<strong>Bold</strong>", ui.Ctrl)
	h.session, h.draft = "second", "new session"
	h.tester.Frame()
	h.converted()
	require(h.draft == "new session", "late conversion overwrote another session")
	fmt.Println("PASS stale conversion and session switch")

	h = newHarness()
	h.paste("Bold", "<strong>Bold</strong>", ui.Ctrl)
	h.tester.Key(0, ui.KeyLeft)
	h.converted()
	h.tester.Type("!")
	require(h.draft == "Bol!d", "late conversion moved the user's caret: "+h.draft)
	fmt.Println("PASS caret movement during conversion")

	h = newHarness()
	h.tester.SetClipboardData(transfer.New(transfer.NewItem(
		transfer.Bytes(transfer.Text, []byte("Bold")), transfer.Bytes(transfer.HTML, []byte("<strong>Bold</strong>")),
	)))
	h.tester.Command("paste")
	h.converted()
	require(h.draft == "**Bold**", "menu paste did not convert HTML")
	original, err := h.tester.ClipboardData().Read(transfer.HTML)
	require(err == nil && string(original) == "<strong>Bold</strong>", "paste changed the clipboard")
	h.tester.Command("undo")
	h.tester.Command("redo")
	h.tester.Type("!")
	require(h.draft == "**Bold**!", "menu redo did not restore the formatted caret: "+h.draft)
	fmt.Println("PASS menu paste and unchanged clipboard")
}
