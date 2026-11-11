package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/transfer"
	"github.com/egoist/mygo/ui"
)

// composer_attachments.go stages daemon-side files on the input card: native
// file picker, clipboard file paste, external drop, removable chips, and a
// submit path that carries them on the prompt `files` channel. It mirrors
// src/app/composer.rs (staged chips above the input, sendable on empty text)
// and src/app/components.rs (no bubble for textless messages).

// draftAttachment is one file the daemon has staged for the next prompt.
// main.go already declares `pendingAttachments []draftAttachment`; the type
// lives here because only this file (plus composer.go wiring) may change.
type draftAttachment struct {
	// Reference is the daemon-issued blob reference, sent as `blob_reference`.
	Reference string
	// Path is the daemon-side absolute path, sent as `path`.
	Path string
	// Mention is the tooltip path, kept with the local message.
	Mention string
	// Name is the basename drawn on the chip and sent as `name`.
	Name  string
	IsDir bool
	// IsImage is decided by extension at import time so render never touches
	// the filesystem, matching ComposerAttachment::is_image.
	IsImage bool
}

// attachmentIsImage mirrors image_preview::image_format_for_name: the chip
// shows a thumbnail tile for these extensions, a file icon otherwise.
func attachmentIsImage(name string) bool {
	extension := strings.ToLower(strings.TrimPrefix(filepath.Ext(name), "."))
	switch extension {
	case "png", "jpg", "jpeg", "webp", "gif", "svg", "bmp",
		"tif", "tiff", "ico", "pnm", "pbm", "pgm", "ppm":
		return true
	}
	return false
}

// pickAttachments opens the native file picker (multi-select) and stages
// every chosen path through the daemon. Directories are not selectable in
// this dialog (mygo switches file/directory mode per dialog), so folders
// arrive via drag-drop instead.
func (a *app) pickAttachments() {
	if a.client == nil {
		a.status = tr("The local service is unavailable")
		return
	}
	sessionID := a.selectedID
	go func() {
		paths, err := mygo.Dialog.Open(mygo.OpenDialogOptions{
			Parent:   a.win,
			Title:    "Attach files",
			Multiple: true,
		})
		if err != nil || len(paths) == 0 {
			return
		}
		for _, path := range paths {
			path := path
			a.post(func() {
				if a.selectedID == sessionID {
					a.importAttachmentPath(path)
				}
			})
		}
	}()
}

// importAttachmentPath stats one local path and asks the daemon to stage it
// via {"type":"importPathAttachment","path":...}. The completion lands on the
// UI thread through post, so render (which reads pendingAttachments) never
// races the transfer. Runs on the UI thread.
func (a *app) importAttachmentPath(localPath string) {
	localPath = strings.TrimSpace(localPath)
	if localPath == "" || a.client == nil {
		return
	}
	for _, existing := range a.pendingAttachments {
		if existing.Path == localPath || existing.Mention == localPath {
			return
		}
	}
	if a.detail == nil && !a.createDraftSession() {
		return
	}
	state := a.stateForSession(a.detail)
	if state.uploads == nil {
		state.uploads = make(map[string]bool)
	}
	if state.uploads[localPath] {
		return
	}
	state.uploads[localPath] = true
	sessionID := a.detail.ID
	name := filepath.Base(localPath)
	client := a.client
	go func() {
		defer a.post(func() { delete(state.uploads, localPath) })
		info, err := os.Stat(localPath)
		if err != nil || (!info.IsDir() && !info.Mode().IsRegular()) {
			a.post(func() { a.status = tr("Cannot attach file: ") + localPath })
			return
		}
		// Rust parity (stage_pasted_attachments): single files over 20 MB are
		// rejected up front instead of stalling the import.
		const maxAttachmentBytes = 20 << 20
		if !info.IsDir() && info.Size() > maxAttachmentBytes {
			a.post(func() { a.status = tr("Cannot attach ") + name + tr(": file is larger than 20 MB") })
			return
		}
		ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
		defer cancel()
		payload, err := client.Request(ctx, map[string]any{
			"type": "importPathAttachment",
			"path": localPath,
		})
		if err != nil {
			a.post(func() { a.status = tr("Cannot attach ") + name + tr(": ") + err.Error() })
			return
		}
		attachment, err := parseImportedAttachment(payload)
		if err != nil {
			a.post(func() { a.status = tr("Cannot attach ") + name + tr(": ") + err.Error() })
			return
		}
		a.post(func() {
			attachments := state.attachments
			if a.selectedID == sessionID {
				attachments = a.pendingAttachments
			}
			for _, existing := range attachments {
				if existing.Reference != "" && existing.Reference == attachment.Reference {
					return
				}
			}
			state.attachments = append(attachments, attachment)
			if a.selectedID == sessionID {
				a.pendingAttachments = state.attachments
			}
		})
	}()
}

// parseImportedAttachment reads {"attachment":{"reference","path","name",
// "is_dir"}} (snake_case) off an importPathAttachment response.
func parseImportedAttachment(payload json.RawMessage) (draftAttachment, error) {
	var response struct {
		Attachment struct {
			Reference string `json:"reference"`
			Path      string `json:"path"`
			Name      string `json:"name"`
			IsDir     bool   `json:"is_dir"`
		} `json:"attachment"`
	}
	if err := json.Unmarshal(payload, &response); err != nil {
		return draftAttachment{}, err
	}
	attachment := response.Attachment
	if attachment.Reference == "" || attachment.Path == "" {
		return draftAttachment{}, errors.New("missing attachment reference")
	}
	if attachment.Name == "" {
		attachment.Name = filepath.Base(attachment.Path)
	}
	return draftAttachment{
		Reference: attachment.Reference,
		Path:      attachment.Path,
		Mention:   attachment.Path,
		Name:      attachment.Name,
		IsDir:     attachment.IsDir,
		IsImage:   !attachment.IsDir && attachmentIsImage(attachment.Name),
	}, nil
}

// importClipboardFiles stages files held by the clipboard (copied from
// Explorer). It returns false when the clipboard holds no files so the caller
// lets the normal text paste through. Image bytes and plain text stay on the
// text path: the daemon protocol used here stages paths only.
func (a *app) importClipboardFiles() bool {
	paths, err := mygo.Clipboard.ReadFiles()
	if err != nil || len(paths) == 0 {
		return false
	}
	for _, path := range paths {
		a.importAttachmentPath(path)
	}
	return true
}

// handleComposerDrop accepts an external file drop on the composer card and
// stages the paths. DropData reports each drop once on the frame after it,
// so the imports spawned here never duplicate. mygo exposes no drop event
// beyond this, and folders dropped here import fine (importPathAttachment
// accepts directories).
func (a *app) handleComposerDrop(card ui.Element) {
	drop, ok := ui.DropData(card, transfer.DropOptions{
		Formats: []transfer.Format{transfer.FileList, transfer.URIList},
	})
	if !ok {
		return
	}
	paths, err := drop.Data.Files()
	if err != nil || len(paths) == 0 {
		return
	}
	for _, path := range paths {
		a.importAttachmentPath(path)
	}
}

// attachButton is the composer's paperclip: a 28-high MenuChip-style affordance
// for the file picker, sitting after the interaction chip like the source's
// quote entry sits after its mode control.
func (a *app) attachButton(c *ui.Context) {
	attach := ui.ButtonBase(c).Label(tr("Attach files")).Tooltip(tr("Attach files")).
		Size(composerChipHeight, composerChipHeight).Radius(composerChipRadius).Center()
	if attach.Hovered() {
		attach.Background(palette.overlay)
	}
	if attach.Pressed() {
		attach.Background(palette.overlayStrong)
	}
	attach.Children(func() {
		icon(c, "paperclip", 14, palette.textTertiary)
	})
	if attach.Clicked() {
		a.pickAttachments()
	}
}

// pendingAttachmentChips is the staged-attachment row between the input and
// the control row: one 28-high, 7-radius chip per file (12 icon, 12 text with
// ellipsis, hover overlay, x to remove). It renders nothing when empty so no
// space is taken, matching the source's conditional row.
func (a *app) pendingAttachmentChips(c *ui.Context) {
	if len(a.pendingAttachments) == 0 {
		return
	}
	ui.Row(c).FillWidth().Padding(2, 14, 0, 14).Wrap().Gap(6).Children(func() {
		for index := range a.pendingAttachments {
			attachment := a.pendingAttachments[index]
			index := index
			chip := ui.Row(c).Height(28).Padding(0, 4, 0, 8).Radius(7).Gap(6).
				AlignItems(ui.Center).Border(1, palette.border).Tooltip(attachment.Path)
			if chip.Hovered() {
				chip.Background(palette.overlay)
			}
			chip.Children(func() {
				switch {
				case attachment.IsDir:
					icon(c, "folder", 12, palette.textSecondary)
				case attachment.IsImage:
					icon(c, "modality-image", 12, palette.textSecondary)
				default:
					brandIcon(c, fileTypeIcon(attachment.Name), 12)
				}
				ui.Row(c).MinWidth(0).MaxWidth(180).Children(func() {
					ui.Text(c, attachment.Name).FontSize(12).FixedLineHeight(16).
						TextColor(palette.textSecondary).SingleLine().Ellipsis("...")
				})
				remove := ui.ButtonBase(c).Label(tr("Remove attachment")).Tooltip(tr("Remove")).
					Size(20, 20).Radius(5).Center()
				if remove.Hovered() {
					remove.Background(palette.overlayStrong)
				}
				remove.Children(func() {
					icon(c, "x", 11, palette.textTertiary)
				})
				if remove.Clicked() {
					a.removePendingAttachment(index)
				}
			})
		}
	})
}

// composerUploading reports in-flight attachment imports for the open
// session. Submit stays disabled until they settle, mirroring
// submission_control's uploads gate with a visible reason.
func (a *app) composerUploading() bool {
	if a.detail == nil {
		return false
	}
	state := a.live[a.detail.ID]
	return state != nil && len(state.uploads) > 0
}

// composerUploadNotice is the one-line importing state under the chips so an
// upload never looks stalled while send is disabled.
func (a *app) composerUploadNotice(c *ui.Context) {
	if !a.composerUploading() {
		return
	}
	ui.Row(c).FillWidth().Padding(0, 14, 6, 14).Gap(6).AlignItems(ui.Center).Children(func() {
		icon(c, "loader-circle", 12, palette.textTertiary)
		ui.Text(c, tr("Importing attachments…")).FontSize(11.5).TextColor(palette.textMuted)
	})
}

// removePendingAttachment drops one staged file by render-time index.
func (a *app) removePendingAttachment(index int) {
	if index < 0 || index >= len(a.pendingAttachments) {
		return
	}
	a.pendingAttachments = append(a.pendingAttachments[:index], a.pendingAttachments[index+1:]...)
}
