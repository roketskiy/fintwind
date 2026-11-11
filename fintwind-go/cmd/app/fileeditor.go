package main

import (
	"bytes"
	"crypto/sha256"
	"os"
	"strings"

	"github.com/egoist/mygo/ui/editor"
)

// A nativeFileEditor is one open file behind a right-panel file tab. Reads
// and the save-time re-read plus write run in background goroutines; the main
// thread only adopts results whose read or save generation still matches. The
// disk copy's sha256 is the baseline every save verifies against, so a read
// that started earlier can never overwrite a newer truth, and mtime games
// cannot hide a change. hasConflict is explicit because an empty server copy
// is a real conflict, not the absence of one.
type nativeFileEditor struct {
	sessionID string
	relative  string
	absolute  string
	text      string
	diskText  string
	diskHash  [32]byte
	hasHash   bool
	// diskCRLF/diskBOM record the disk copy's format. The MyGo editor
	// buffer holds LF text only (ui/editor/buffer.go String is LF,
	// WriteTo restores the recorded ending), so dirty compares the
	// normalized text while the write path encodes back. Main thread only.
	diskCRLF    bool
	diskBOM     bool
	loaded      bool
	loading     bool
	saving      bool
	readGen     uint64
	saveGen     uint64
	readErr     string
	saveErr     string
	conflict    string
	hasConflict bool
	// edState owns the MyGo editor widget state (ui/editor.State, kept by
	// the app across frames per its docs). bufText is the last text pushed
	// into or pulled from the buffer, so external adoptions and user edits
	// sync without clobbering each other. Main thread only.
	edState *editor.State
	bufText string
	hasBuf  bool
}

// Stable automation labels for the file editor surface.
const (
	nativeFileEditorLabel = "native-file-editor"
	nativeFileSaveLabel   = "native-file-save"
	nativeFileErrorLabel  = "native-file-error"
)

// hashBytes returns the conflict-guard digest of one disk copy.
func hashBytes(data []byte) [32]byte {
	return sha256.Sum256(data)
}

// utf8BOM is the prefix a BOM-carrying file starts with.
var utf8BOM = []byte{0xEF, 0xBB, 0xBF}

// normalizeDiskBytes splits a raw disk copy into the LF-normalized draft
// the editor buffer holds plus the format flags the write path restores.
// Stripping here is what keeps a CRLF file from reading as dirty.
func normalizeDiskBytes(data []byte) (text string, crlf, bom bool) {
	rest := data
	if bytes.HasPrefix(rest, utf8BOM) {
		bom = true
		rest = rest[len(utf8BOM):]
	}
	text = string(rest)
	if strings.Contains(text, "\r\n") {
		crlf = true
		text = strings.ReplaceAll(text, "\r\n", "\n")
	}
	return text, crlf, bom
}

// encodeDiskBytes renders an LF draft back in the disk format the flags
// describe, so a save never rewrites line endings or drops a BOM.
func encodeDiskBytes(draft string, crlf, bom bool) []byte {
	if crlf {
		draft = strings.ReplaceAll(draft, "\n", "\r\n")
	}
	raw := []byte(draft)
	if !bom {
		return raw
	}
	out := make([]byte, 0, len(utf8BOM)+len(raw))
	out = append(out, utf8BOM...)
	out = append(out, raw...)
	return out
}

// openNativeFile points the session entry at a file tab and reads it off the
// render path. absolute must already be resolved on the main thread. An
// editor that already holds content (loaded or still loading) keeps it: a
// second open never re-reads over unsaved edits. Main thread only.
func (a *app) openNativeFile(sessionID, relative, absolute string) *nativeFileEditor {
	entry := a.nativePanelsFor(sessionID)
	editor, ok := entry.files[relative]
	if !ok {
		editor = &nativeFileEditor{sessionID: sessionID, relative: relative}
		entry.files[relative] = editor
	}
	editor.absolute = absolute
	if editor.loaded || editor.loading {
		return editor
	}
	editor.readGen++
	gen := editor.readGen
	editor.loading = true
	editor.readErr = ""
	go func() {
		data, readErr := os.ReadFile(absolute)
		var diskHash [32]byte
		var hasHash bool
		if readErr == nil {
			diskHash = hashBytes(data)
			hasHash = true
		}
		a.post(func() {
			a.finishNativeFileRead(sessionID, relative, gen, absolute, data, diskHash, hasHash, readErr)
		})
	}()
	return editor
}

// finishNativeFileRead adopts a fresh read when its generation, session, and
// path still match. A read for a generation the editor already passed (a
// newer open, or edits in flight) is dropped, so late bytes never overwrite
// an unsaved draft. Runs on the main thread inside a.post.
func (a *app) finishNativeFileRead(sessionID, relative string, gen uint64, absolute string, data []byte, diskHash [32]byte, hasHash bool, readErr error) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	editor, ok := entry.files[relative]
	if !ok || editor.readGen != gen || editor.absolute != absolute {
		return
	}
	editor.loading = false
	if readErr != nil {
		editor.readErr = readErr.Error()
		return
	}
	text, crlf, bom := normalizeDiskBytes(data)
	editor.text = text
	editor.diskText = text
	editor.diskHash = diskHash
	editor.hasHash = hasHash
	editor.diskCRLF = crlf
	editor.diskBOM = bom
	editor.loaded = true
	editor.readErr = ""
	editor.conflict = ""
	editor.hasConflict = false
	editor.pushToBuffer(text)
}

// setNativeFileText records an edit from the main thread. The disk baseline
// is untouched until a verified save lands.
func (a *app) setNativeFileText(sessionID, relative, text string) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	if editor, ok := entry.files[relative]; ok {
		editor.text = text
	}
}

// syncNativeFileBuffer keeps the widget and the draft in step. User typing
// pulls from the buffer into the draft; an external adoption pushes through
// pushToBuffer first and the buffer follows. Call it around the widget
// build, on the main thread.
func (ed *nativeFileEditor) syncNativeFileBuffer() {
	state := ed.editorState()
	if state.Buffer == nil {
		return
	}
	if current := state.Buffer.String(); current != ed.bufText {
		ed.text = current
		ed.bufText = current
		return
	}
	if ed.text != ed.bufText {
		state.Buffer.Reset(ed.text)
		ed.bufText = ed.text
	}
}

// editorState creates the widget state on first use, seeded from the draft
// with the file's language for highlighting.
func (ed *nativeFileEditor) editorState() *editor.State {
	if ed.edState == nil {
		ed.edState = &editor.State{
			Buffer:      editor.NewBuffer(ed.text),
			LineNumbers: editor.LineNumbersOn,
		}
		if language := editor.LanguageFor(ed.relative); language != nil {
			ed.edState.Buffer.SetLanguage(language)
		}
		ed.bufText = ed.text
		ed.hasBuf = true
	}
	return ed.edState
}

// pushToBuffer replaces the widget text after an external adoption. The
// draft already holds the same text; the buffer follows it.
func (ed *nativeFileEditor) pushToBuffer(text string) {
	ed.text = text
	ed.bufText = text
	ed.hasBuf = true
	if ed.edState != nil && ed.edState.Buffer != nil {
		ed.edState.Buffer.Reset(text)
	}
}

// nativeFileDirty reports whether the draft differs from the verified disk
// copy. An unread file counts as clean, never as an empty file.
func (ed *nativeFileEditor) nativeFileDirty() bool {
	if ed == nil || !ed.loaded {
		return false
	}
	return ed.text != ed.diskText
}

// saveNativeFile writes the draft after a save-time re-read proves the disk
// copy is unchanged. A changed disk copy raises a conflict and nothing is
// written. Guards against double submit while a save is in flight. The whole
// check plus write runs in the background: the main thread never touches the
// disk here.
func (a *app) saveNativeFile(sessionID, relative string) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	editor, ok := entry.files[relative]
	if !ok || editor.saving || editor.loading || !editor.loaded {
		return
	}
	if !editor.nativeFileDirty() {
		return
	}
	editor.saving = true
	editor.saveErr = ""
	editor.saveGen++
	gen := editor.saveGen
	absolute := editor.absolute
	draft := editor.text
	wantHash, wantHas := editor.diskHash, editor.hasHash
	wantCRLF, wantBOM := editor.diskCRLF, editor.diskBOM
	go func() {
		result := checkAndWriteNativeFile(absolute, draft, wantHash, wantHas, false, wantCRLF, wantBOM)
		a.post(func() {
			a.finishNativeFileSave(sessionID, relative, gen, absolute, draft, result)
		})
	}()
}

// saveAnywayNativeFile writes the draft without the disk guard, after the
// user explicitly chose overwrite on a conflict dialog. The write still runs
// in the background.
func (a *app) saveAnywayNativeFile(sessionID, relative string) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	editor, ok := entry.files[relative]
	if !ok || editor.saving || editor.loading || !editor.loaded || !editor.hasConflict {
		return
	}
	editor.saving = true
	editor.saveErr = ""
	editor.saveGen++
	gen := editor.saveGen
	absolute := editor.absolute
	draft := editor.text
	wantHash, wantHas := editor.diskHash, editor.hasHash
	wantCRLF, wantBOM := editor.diskCRLF, editor.diskBOM
	go func() {
		result := checkAndWriteNativeFile(absolute, draft, wantHash, wantHas, true, wantCRLF, wantBOM)
		a.post(func() {
			a.finishNativeFileSave(sessionID, relative, gen, absolute, draft, result)
		})
	}()
}

// nativeFileSaveResult is the background save outcome the main thread adopts.
type nativeFileSaveResult struct {
	changed   bool
	server    []byte
	serverErr string
	writeErr  string
	diskHash  [32]byte
	hasHash   bool
}

// checkAndWriteNativeFile runs entirely off the main thread: re-read the
// disk, compare hashes, and (unless the disk moved or force is set) write.
// The draft is LF-normalized editor text; it is encoded back to the disk
// format before hashing comparisons of the write or the write itself, so a
// CRLF/BOM file round-trips byte-identical. It never touches editor state.
func checkAndWriteNativeFile(absolute, draft string, wantHash [32]byte, wantHas, force, crlf, bom bool) nativeFileSaveResult {
	data, readErr := os.ReadFile(absolute)
	if readErr != nil {
		return nativeFileSaveResult{changed: true, serverErr: readErr.Error()}
	}
	diskHash := hashBytes(data)
	if !force && (!wantHas || diskHash != wantHash) {
		return nativeFileSaveResult{changed: true, server: data}
	}
	if err := os.WriteFile(absolute, encodeDiskBytes(draft, crlf, bom), 0o644); err != nil {
		return nativeFileSaveResult{writeErr: err.Error()}
	}
	fresh, statErr := os.ReadFile(absolute)
	if statErr != nil {
		return nativeFileSaveResult{writeErr: statErr.Error()}
	}
	return nativeFileSaveResult{diskHash: hashBytes(fresh), hasHash: true}
}

// finishNativeFileSave commits a verified write or records the server copy a
// conflict dialog shows. Runs on the main thread inside a.post; it only
// adopts state, never touches the disk.
func (a *app) finishNativeFileSave(sessionID, relative string, gen uint64, absolute, draft string, result nativeFileSaveResult) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	editor, ok := entry.files[relative]
	if !ok || editor.saveGen != gen || editor.absolute != absolute {
		return
	}
	editor.saving = false
	if result.changed {
		if result.serverErr != "" {
			editor.saveErr = result.serverErr
			return
		}
		editor.conflict = string(result.server)
		editor.hasConflict = true
		return
	}
	if result.writeErr != "" {
		editor.saveErr = result.writeErr
		return
	}
	// The write encoded the draft back into the recorded disk format, so
	// the normalized draft is the new baseline and the flags stand.
	editor.diskText = draft
	editor.diskHash = result.diskHash
	editor.hasHash = result.hasHash
	editor.conflict = ""
	editor.hasConflict = false
}

// reloadNativeFileFromDisk replaces the draft with the conflict's server
// copy after the user chooses it. The conflict clears and the baseline moves
// to the server text; the widget follows through pushToBuffer.
func (a *app) reloadNativeFileFromDisk(sessionID, relative string) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	editor, ok := entry.files[relative]
	if !ok || !editor.hasConflict {
		return
	}
	text, crlf, bom := normalizeDiskBytes([]byte(editor.conflict))
	editor.diskText = text
	editor.diskHash = hashBytes([]byte(editor.conflict))
	editor.hasHash = true
	editor.diskCRLF = crlf
	editor.diskBOM = bom
	editor.conflict = ""
	editor.hasConflict = false
	editor.pushToBuffer(editor.diskText)
}

// dismissNativeFileConflict closes the conflict without touching the draft.
// The next save re-reads and raises it again if the disk still differs.
func (a *app) dismissNativeFileConflict(sessionID, relative string) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	if editor, ok := entry.files[relative]; ok {
		editor.conflict = ""
		editor.hasConflict = false
	}
}

// closeNativeFile drops one file tab's editor. An in-flight read or save
// lands on a missing entry and is dropped by its generation guard.
func (a *app) closeNativeFile(sessionID, relative string) {
	entry, ok := a.native.sessions[sessionID]
	if !ok {
		return
	}
	delete(entry.files, relative)
}
