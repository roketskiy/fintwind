package main

import (
	"fmt"
	"sort"
	"strings"

	"fintwind-go/assets"
	"github.com/egoist/mygo/ui"
	"github.com/lobehub/lobe-mygo/pkg/action"
	"github.com/lobehub/lobe-mygo/pkg/field"
	"github.com/lobehub/lobe-mygo/pkg/overlay"
	"github.com/lobehub/lobe-mygo/pkg/theme"
)

// The live area of the source control page: workspace status, the commit
// box and the change tree in the secondary column, the diff reader in the
// main area. A turn or range snapshot takes the whole page instead
// (review.go, review_view.go).

func (a *app) sourceControlView(c *ui.Context) {
	if c.Shortcut(0, ui.KeyEscape) || c.Shortcut(ui.Ctrl, ui.KeyBracketLeft) {
		// Leave the page, keep the review
		// state for the next visit.
		a.mode = modeSessions
	}
	if !a.review.Active {
		a.scEnsureFresh()
	}
	themed := lobeThemed(c, func() {
		if a.review.Active {
			a.reviewView(c)
			return
		}
		ui.Row(c).FillWidth().Grow(1).MinWidth(0).FillHeight().Children(func() {
			if a.sidebarOpen {
				secondary := ui.Column(c).Width(a.scSecWidth).FillHeight().Shrink(0).
					Background(palette.sidebar).Radius(workspaceRadius).Clip()
				secondary = sidebarShadow(secondary)
				secondary.Children(func() {
					a.scSecondary(c)
					a.secondaryResizeHandle(c, &a.scSecWidth)
				})
			}
			a.scMain(c)
		})
	})
	// The themed wrapper is a plain column; without an explicit fill it
	// shrinks to its content and the whole page reflows whenever a label
	// changes width.
	themed.FillWidth().FillHeight().Grow(1).MinWidth(0).Background(palette.rail)
}

func (a *app) scSecondary(c *ui.Context) {
	st := &a.sourceControl
	// Workspace block: project directory + branch row, like
	// render_source_control_workspace_block.
	ui.Column(c).FillWidth().Padding(12, 12, 10, 12).Gap(4).Children(func() {
		ui.Text(c, projectBaseName(st.Root)).FontSize(uiPx(12.5)).TextColor(palette.textSecondary).SingleLine().Ellipsis("...").Tooltip(st.Root)
		if st.Loading && st.Root == "" {
			reviewNote(c, tr("Reading Git status…"), false)
		}
		if st.Error != "" {
			reviewNote(c, st.Error, true)
		}
		if st.Notice != "" {
			reviewNote(c, st.Notice, false)
		}
		if write := st.writeState(); write != nil {
			if write.Busy {
				reviewNote(c, trf("%s…", write.Label), false)
			}
			if write.Unknown {
				reviewNote(c, tr("Git result unknown. Refresh status and inspect history before another write."), true)
			}
		}
		a.scBranchRow(c)
	})
	// VSCode-style commit box, always visible.
	a.scCommitBox(c)
	if st.Discard != nil {
		a.scDiscardDialog(c)
	}
	ui.Column(c).FillWidth().Padding(4, 12).Children(func() {
		sidebarFilterSearch(c, "sc-files", tr("Filter files"), &st.Filter)
		if st.Filter != st.AppliedFilter {
			st.rebuildRows()
		}
	})
	if len(st.ListRows) == 0 {
		label := tr("No uncommitted changes")
		if st.Scope.Client == nil {
			label = tr("No local workspace selected")
		} else if !st.IsRepo && st.Loaded && st.Error == "" {
			label = tr("Not a Git repository")
		} else if st.Filter != "" {
			label = tr("No matching files")
		}
		reviewNote(c, label, false)
		return
	}
	a.scFileList(c)
}

// scBranchRow mirrors render_source_control_branch_row: branch chip,
// refresh, repository menu (fetch/pull/push).
func (a *app) scBranchRow(c *ui.Context) {
	st := &a.sourceControl
	branchLabel := st.Branch
	if branchLabel == "" {
		branchLabel = tr("Detached HEAD")
	}
	if st.Ahead > 0 {
		branchLabel += fmt.Sprintf(" ↑%d", st.Ahead)
	}
	if st.Behind > 0 {
		branchLabel += fmt.Sprintf(" ↓%d", st.Behind)
	}
	ui.Row(c).FillWidth().Gap(4).AlignItems(ui.Center).Children(func() {
		chip := action.Button(c, "", action.ButtonProps{Variant: action.Default, Size: theme.Small})
		chip.Label(branchLabel).Grow(1).MinWidth(0)
		if !st.writable() {
			chip.Disabled(true).Opacity(0.4)
		}
		chip.Children(func() {
			icon(c, "git-branch", 12, palette.textTertiary)
			ui.Text(c, branchLabel).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).Grow(1).MinWidth(0).SingleLine().Ellipsis("...")
			icon(c, "chevron-down", 10, palette.textGhost)
		})
		if chip.Clicked() && st.writable() {
			if st.BranchOpen {
				st.BranchOpen = false
			} else {
				a.scLoadBranches()
			}
		}
		refresh := action.ActionIcon(c, assets.SVG("rotate-cw"), action.ActionIconProps{Size: theme.Small, Title: tr("Refresh")})
		if !st.writable() && st.Loading {
			refresh.Disabled(true).Opacity(0.4)
		} else {
			refresh.OnClick(func() { a.scRefreshSurface() })
		}
		a.scRepoMenu(c)
		a.scBranchPopover(c, chip)
	})
}

// scBranchPopover drops the branch picker down from the chip: filter, create,
// and the branch list, closing on an outside click instead of taking over the
// column.
func (a *app) scBranchPopover(c *ui.Context, chip ui.Element) {
	st := &a.sourceControl
	open := st.BranchOpen
	ui.PopoverBase(c, chip, &open, func(panel ui.Element) {
		panel.Background(palette.raised).Border(1, palette.borderStrong).Radius(10).
			Width(250).Padding(8).Gap(6).Shadow(0, 2, 8, 0, ui.RGBA(0, 0, 0, 0.28)).Children(func() {
			sidebarFilterSearch(c, "sc-branches", tr("Filter branches"), &st.BranchFilter)
			c.Key("sc-branch-create")
			create := field.Input(c, &st.BranchCreate, field.InputProps{Placeholder: tr("New branch name…"), Size: theme.Small})
			create.Label(tr("Create branch"))
			c.OnShortcut(ui.Ctrl, ui.KeyEnter, func() {
				name := strings.TrimSpace(st.BranchCreate)
				if name != "" && st.writable() {
					a.scMutate(tr("Create branch"), map[string]any{"type": "checkoutBranch", "branch": name, "create": true})
				}
			})
			if st.BranchLoading {
				reviewNote(c, tr("Reading branches…"), false)
				return
			}
			st.filterBranches()
			branches := st.BranchRows
			if len(branches) == 0 {
				return
			}
			c.Key("sc-branches")
			height := min(280, float32(len(branches))*scRowHeight+8)
			ui.List(c, &st.BranchList, len(branches)).FillWidth().Height(height).ItemKey(func(i int) any { return branches[i].Name }).Rows(func(row ui.ListRow) {
				branch := branches[row.Index]
				c.Key(branch.Name)
				disabled := branch.Elsewhere || branch.Name == st.Branch || !st.writable()
				tip := branch.Name
				if branch.Elsewhere {
					tip = tr("Checked out in another worktree")
				}
				entry := ui.ButtonBase(c).Label(branch.Name).Tooltip(tip).FillWidth().MinHeight(scRowHeight).Padding(0, 8).Radius(6).Gap(8).AlignItems(ui.Center)
				if disabled {
					entry.Disabled(true).Opacity(0.4)
				} else {
					if entry.Hovered() {
						entry.Background(palette.overlay)
					}
					if entry.Pressed() {
						entry.Background(palette.overlayStrong)
					}
				}
				entry.Children(func() {
					icon(c, "git-branch", 12, palette.textGhost)
					ui.Text(c, branch.Name).FontSize(uiPx(12)).TextColor(palette.text).Grow(1).MinWidth(0).SingleLine().Ellipsis("...")
					if branch.Name == st.Branch {
						icon(c, "check", 12, palette.successText)
					}
				})
				if !disabled {
					entry.OnClick(func() {
						a.scMutate(tr("Switch branch"), map[string]any{"type": "checkoutBranch", "branch": branch.Name, "create": false})
					})
				}
			})
		})
	})
	st.BranchOpen = open
}

// scRepoMenu mirrors render_source_control_repository_menu: fetch, pull,
// push behind an ellipsis trigger.
func (a *app) scRepoMenu(c *ui.Context) {
	st := &a.sourceControl
	writable := st.writable()
	items := []overlay.MenuItem{
		{Key: "fetch", Label: tr("Fetch"), Icon: assets.SVG("download"), Disabled: !writable},
		{Key: "pull", Label: tr("Pull"), Icon: assets.SVG("arrow-down"), Disabled: !writable},
		{Key: "push", Label: tr("Push"), Icon: assets.SVG("arrow-up"), Disabled: !writable},
	}
	chosen := overlay.DropdownMenu(c, &st.RepoMenuOpen, overlay.DropdownMenuProps{Items: items}, func(t ui.Element) {
		action.StyleActionIcon(c, t, assets.SVG("ellipsis"), action.ActionIconProps{Size: theme.Small, Title: tr("Repository actions")})
	})
	switch chosen {
	case "fetch":
		st.RepoMenuOpen = false
		a.scMutate(tr("Fetch"), map[string]any{"type": "fetchRemote"})
	case "pull":
		st.RepoMenuOpen = false
		a.scMutate(tr("Pull"), map[string]any{"type": "pullFastForward"})
	case "push":
		st.RepoMenuOpen = false
		a.scMutate(tr("Push"), map[string]any{"type": "push"})
	}
}

// scCommitBox mirrors render_source_control_commit_box: message input with
// Ctrl+Enter, primary commit/pull/push button + dropdown.
func (a *app) scCommitBox(c *ui.Context) {
	st := &a.sourceControl
	writable := st.writable()
	hasMessage := strings.TrimSpace(st.CommitMessage) != ""
	dirty := len(st.Files) > 0
	canCommit := writable && st.HasStaged && hasMessage
	canStageCommit := writable && hasMessage && dirty
	// The primary button matches the workspace: stage-and-commit when only
	// unstaged changes sit there, pull what is behind, push what is ahead.
	primaryLabel, primaryIcon, primaryReady := tr("Commit"), "check", canCommit
	switch {
	case !writable:
		primaryReady = false
	case !dirty:
		if st.Behind > 0 {
			primaryLabel, primaryIcon, primaryReady = tr("Pull"), "arrow-down", true
		} else if st.Ahead > 0 {
			primaryLabel, primaryIcon, primaryReady = tr("Push"), "arrow-up", true
		} else {
			primaryReady = false
		}
	case !st.HasStaged:
		primaryLabel, primaryIcon = tr("Stage all and commit"), "plus"
		primaryReady = hasMessage
	}
	placeholder := tr("Message")
	if st.Branch != "" {
		placeholder = trf("Message (%s)", st.Branch)
	}
	ui.Column(c).FillWidth().Padding(10, 12, 8, 12).Children(func() {
		ui.Column(c).FillWidth().Radius(12).Border(1, palette.borderStrong).Background(palette.raised).Padding(10).Gap(8).Children(func() {
			c.Key("sc-commit-message")
			msg := field.TextArea(c, &st.CommitMessage, field.TextAreaProps{Placeholder: placeholder, MinRows: 2, MaxRows: 10, Variant: field.InputOutlined})
			msg.Label(tr("Commit message")).ReadOnly(!writable)
			c.OnShortcut(ui.Ctrl, ui.KeyEnter, func() { a.scAcceptInput() })
			ui.Row(c).FillWidth().Gap(6).Children(func() {
				primary := action.Button(c, "", action.ButtonProps{Variant: choice(primaryReady, action.Primary, action.Filled), Size: theme.Middle})
				primary.Label(primaryLabel).Grow(1).MinWidth(0)
				if !primaryReady {
					primary.Disabled(true)
				} else {
					primary.OnClick(func() { a.scAcceptInput() })
				}
				primary.Children(func() {
					icon(c, primaryIcon, 13, palette.onAccent)
					ui.Text(c, primaryLabel).FontSize(uiPx(12.5))
				})
				a.scCommitMore(c, writable, canCommit, canStageCommit)
			})
		})
	})
}

// scCommitMore is the commit box's dropdown: commit variants and the
// network actions the primary button falls back to.
func (a *app) scCommitMore(c *ui.Context, writable, canCommit, canStageCommit bool) {
	st := &a.sourceControl
	items := []overlay.MenuItem{
		{Key: "commit", Label: tr("Commit"), Icon: assets.SVG("check"), Disabled: !canCommit},
		{Key: "commit-push", Label: tr("Commit and push"), Icon: assets.SVG("arrow-up"), Disabled: !canCommit},
		{Key: "stage-commit", Label: tr("Stage all and commit"), Icon: assets.SVG("plus"), Disabled: !canStageCommit},
		{Key: "stage-commit-push", Label: tr("Stage all, commit and push"), Icon: assets.SVG("cloud-upload"), Disabled: !canStageCommit},
		{Key: "sep", Divider: true},
		{Key: "pull", Label: tr("Pull"), Icon: assets.SVG("arrow-down"), Disabled: !writable},
		{Key: "push", Label: tr("Push"), Icon: assets.SVG("arrow-up"), Disabled: !writable},
		{Key: "fetch", Label: tr("Fetch"), Icon: assets.SVG("download"), Disabled: !writable},
	}
	chosen := overlay.DropdownMenu(c, &st.CommitMoreOpen, overlay.DropdownMenuProps{Items: items}, func(t ui.Element) {
		action.StyleActionIcon(c, t, assets.SVG("chevron-down"), action.ActionIconProps{Size: theme.Small, Title: tr("More commit actions")})
	})
	switch chosen {
	case "commit":
		st.CommitMoreOpen = false
		a.scCommit(false, false)
	case "commit-push":
		st.CommitMoreOpen = false
		a.scCommit(false, true)
	case "stage-commit":
		st.CommitMoreOpen = false
		a.scCommit(true, false)
	case "stage-commit-push":
		st.CommitMoreOpen = false
		a.scCommit(true, true)
	case "pull":
		st.CommitMoreOpen = false
		a.scMutate(tr("Pull"), map[string]any{"type": "pullFastForward"})
	case "push":
		st.CommitMoreOpen = false
		a.scMutate(tr("Push"), map[string]any{"type": "push"})
	case "fetch":
		st.CommitMoreOpen = false
		a.scMutate(tr("Fetch"), map[string]any{"type": "fetchRemote"})
	}
}

// scAcceptInput runs the primary commit-box action for Ctrl+Enter, matching
// the button the workspace shows: stage-and-commit with nothing staged,
// commit while staged, otherwise pull-then-push order.
func (a *app) scAcceptInput() {
	st := &a.sourceControl
	if !st.writable() {
		return
	}
	if len(st.Files) > 0 {
		a.scCommit(!st.HasStaged, false)
		return
	}
	if st.Behind > 0 {
		a.scMutate(tr("Pull"), map[string]any{"type": "pullFastForward"})
	} else if st.Ahead > 0 {
		a.scMutate(tr("Push"), map[string]any{"type": "push"})
	}
}

// scListRow is one row of the change tree: a group header, a foldable
// directory, a file, or the ignored block.
type scListRow struct {
	Kind  byte // 'g' group header, 'd' directory, 'f' file, 'i' ignored header, 'n' ignored file
	Group string
	Dir   string
	Name  string
	File  scFile
	Depth int
	Count int
}

func (r scListRow) key() string {
	switch r.Kind {
	case 'g':
		return "g\x00" + r.Group
	case 'd':
		return "d\x00" + r.Group + "\x00" + r.Dir
	case 'i':
		return "ignored"
	case 'n':
		return "n\x00" + r.File.Path
	}
	return "f\x00" + r.File.key()
}

// scTreeDir is one directory level of the change tree.
type scTreeDir struct {
	dirs     map[string]*scTreeDir
	dirNames []string
	files    []scFile
}

func scTreeAdd(root *scTreeDir, f scFile) {
	dir := root
	if i := strings.LastIndex(f.Path, "/"); i >= 0 {
		for _, seg := range strings.Split(f.Path[:i], "/") {
			child, ok := dir.dirs[seg]
			if !ok {
				child = &scTreeDir{dirs: map[string]*scTreeDir{}}
				dir.dirs[seg] = child
				dir.dirNames = append(dir.dirNames, seg)
			}
			dir = child
		}
	}
	dir.files = append(dir.files, f)
}

// scTreeEmit flattens the tree into list rows: directories at their depth,
// their children one step in, directories before files, names sorted. The
// filter forces every directory open.
func scTreeEmit(group string, dir *scTreeDir, prefix string, depth int, forced bool, collapsed map[string]bool, rows *[]scListRow) {
	sort.Strings(dir.dirNames)
	for _, name := range dir.dirNames {
		full := name
		if prefix != "" {
			full = prefix + "/" + name
		}
		*rows = append(*rows, scListRow{Kind: 'd', Group: group, Dir: full, Name: name, Depth: depth})
		if forced || !collapsed[group+"\x00"+full] {
			scTreeEmit(group, dir.dirs[name], full, depth+1, forced, collapsed, rows)
		}
	}
	sort.Slice(dir.files, func(i, j int) bool { return dir.files[i].Path < dir.files[j].Path })
	for _, f := range dir.files {
		*rows = append(*rows, scListRow{Kind: 'f', Group: group, File: f, Depth: depth})
	}
}

// buildListRows flattens the change tree the list renders: group headers
// with their actions, then the nested directory tree. It runs from
// rebuildRows and from the fold and ignored toggles.
func (st *sourceControlState) buildListRows() {
	st.ListRows = st.ListRows[:0]
	query := strings.ToLower(st.Filter)
	forced := st.Filter != ""
	for _, group := range []string{"staged", "changes", "untracked"} {
		var files []scFile
		for _, f := range st.Files {
			if f.Group == group && strings.Contains(strings.ToLower(f.Path), query) {
				files = append(files, f)
			}
		}
		if len(files) == 0 {
			continue
		}
		st.ListRows = append(st.ListRows, scListRow{Kind: 'g', Group: group, Count: len(files)})
		if forced || !st.CollapsedGroups[group] {
			root := &scTreeDir{dirs: map[string]*scTreeDir{}}
			for _, f := range files {
				scTreeAdd(root, f)
			}
			scTreeEmit(group, root, "", 0, forced, st.CollapsedDirs, &st.ListRows)
		}
	}
	if st.IgnoredOpen && st.Root != "" {
		st.ListRows = append(st.ListRows, scListRow{Kind: 'i'})
		for _, file := range st.Ignored {
			if st.Filter != "" && !strings.Contains(strings.ToLower(file.Path), query) {
				continue
			}
			st.ListRows = append(st.ListRows, scListRow{Kind: 'n', File: file, Depth: 1})
		}
	}
}

func (a *app) scFileList(c *ui.Context) {
	st := &a.sourceControl
	st.FileList.Selected = nil
	c.Key("sc-file-list")
	ui.List(c, &st.FileList, len(st.ListRows)).FillWidth().Grow(1).MinHeight(0).
		ItemKey(func(i int) any { return st.ListRows[i].key() }).
		Rows(func(row ui.ListRow) {
			a.scListRowView(c, st.ListRows[row.Index])
		})
}

// scListRowView draws one row of the change tree.
func (a *app) scListRowView(c *ui.Context, row scListRow) {
	st := &a.sourceControl
	switch row.Kind {
	case 'g':
		c.Key(row.key())
		writable := st.writable()
		ui.Row(c).FillWidth().MinHeight(scRowHeight).Padding(5, 12, 5, 8).Gap(6).AlignItems(ui.Center).Children(func() {
			fold := ui.ButtonBase(c).Label(scGroupLabel(row.Group)).Size(20, 20).Radius(5).Center()
			if fold.Hovered() {
				fold.Background(palette.overlay)
			}
			fold.Children(func() {
				if st.CollapsedGroups[row.Group] {
					icon(c, "chevron-right", 10, palette.textGhost)
				} else {
					icon(c, "chevron-down", 10, palette.textGhost)
				}
			})
			// OnClick, not Clicked: rebuilding the rows must wait until the
			// frame ends, or the list still iterating them reads past the
			// shortened slice and panics.
			fold.OnClick(func() {
				if st.CollapsedGroups == nil {
					st.CollapsedGroups = map[string]bool{}
				}
				st.CollapsedGroups[row.Group] = !st.CollapsedGroups[row.Group]
				st.buildListRows()
			})
			ui.Text(c, fmt.Sprintf(tr("%s (%d)"), scGroupLabel(row.Group), row.Count)).FontSize(uiPx(11)).TextColor(scGroupColor(row.Group))
			ui.Row(c).Grow(1).Children(func() {})
			if writable {
				if row.Group == "staged" {
					a.scGroupAction(c, row.Group, tr("Unstage all"), true)
				} else {
					a.scGroupAction(c, row.Group, tr("Stage all"), false)
				}
			}
		})
	case 'd':
		c.Key(row.key())
		key := row.Group + "\x00" + row.Dir
		open := !st.CollapsedDirs[key]
		entry := ui.ButtonBase(c).Label(row.Dir).FillWidth().MinHeight(scRowHeight).Padding(5, 12).Gap(6).AlignItems(ui.Center)
		if entry.Hovered() {
			entry.Background(palette.overlay)
		}
		if entry.Pressed() {
			entry.Background(palette.overlayStrong)
		}
		entry.Children(func() {
			ui.Row(c).Width(float32(16 * row.Depth)).Shrink(0).Children(func() {})
			if open {
				icon(c, "chevron-down", 10, palette.textGhost)
			} else {
				icon(c, "chevron-right", 10, palette.textGhost)
			}
			icon(c, "folder", 13, palette.textGhost)
			ui.Text(c, row.Name).FontSize(uiPx(11.5)).TextColor(palette.textTertiary).Grow(1).MinWidth(0).SingleLine().Ellipsis("...")
		})
		entry.OnClick(func() {
			st.CollapsedDirs[key] = open
			st.buildListRows()
		})
	case 'i':
		c.Key("ignored")
		entry := ui.ButtonBase(c).Label(tr("Ignored")).FillWidth().MinHeight(scRowHeight).Padding(5, 12, 5, 8).Gap(6).AlignItems(ui.Center)
		if entry.Hovered() {
			entry.Background(palette.overlay)
		}
		entry.Children(func() {
			if st.IgnoredOpen {
				icon(c, "chevron-down", 10, palette.textGhost)
			} else {
				icon(c, "chevron-right", 10, palette.textGhost)
			}
			ui.Text(c, tr("Ignored")).FontSize(uiPx(11)).TextColor(palette.textTertiary)
			if st.IgnoredLoading {
				ui.Text(c, tr("Loading…")).FontSize(uiPx(11)).TextColor(palette.textTertiary)
			}
		})
		entry.OnClick(func() {
			st.IgnoredOpen = !st.IgnoredOpen
			if st.IgnoredOpen && len(st.Ignored) == 0 && !st.IgnoredLoading {
				a.scLoadIgnored()
			}
			st.buildListRows()
		})
	case 'n':
		c.Key(row.key())
		file := row.File
		ui.Row(c).FillWidth().MinHeight(scRowHeight).Padding(5, 12).Gap(6).AlignItems(ui.Center).Children(func() {
			ui.Row(c).Width(float32(10 + 16*row.Depth)).Shrink(0).Children(func() {})
			brandIcon(c, fileTypeIcon(file.Path), 13)
			name := file.Path
			if index := strings.LastIndex(file.Path, "/"); index >= 0 {
				name = file.Path[index+1:]
			}
			ui.Text(c, name).FontSize(uiPx(11.5)).TextColor(palette.textTertiary).Grow(1).MinWidth(0).SingleLine().Ellipsis("...").Tooltip(file.Path)
		})
	default:
		a.scFileRow(c, row.File, row.Depth)
	}
}

// scGroupAction is a group header's stage-all or unstage-all action, at the
// muted contrast the header's label has.
func (a *app) scGroupAction(c *ui.Context, group, label string, unstage bool) {
	c.Key("sc-group-action-" + group)
	b := reviewMutedButton(c, label)
	b.OnClick(func() {
		st := &a.sourceControl
		paths := make([]string, 0, len(st.Files))
		for _, f := range st.Files {
			if f.Group != group || !strings.Contains(strings.ToLower(f.Path), strings.ToLower(st.Filter)) {
				continue
			}
			if unstage || !strings.HasSuffix(f.Path, "/") {
				paths = append(paths, f.Path)
			}
		}
		if unstage {
			a.scMutate(tr("Unstage"), map[string]any{"type": "unstagePaths", "paths": paths})
		} else {
			a.scMutate(tr("Stage"), map[string]any{"type": "stagePaths", "paths": paths})
		}
	})
}

// scFileRow is one clickable file with its status letter, indented one step per tree level.
func (a *app) scFileRow(c *ui.Context, file scFile, depth int) {
	st := &a.sourceControl
	c.Key(file.key())
	entry := ui.ButtonBase(c).Label(scFileLabel(file)).FillWidth().MinHeight(scRowHeight).Padding(5, 12).Gap(6).AlignItems(ui.Center)
	if st.Selected == file.key() {
		entry.Background(palette.overlayStrong)
	} else if entry.Hovered() {
		entry.Background(palette.overlay)
	}
	if entry.Pressed() {
		entry.Background(palette.overlayStrong)
	}
	entry.Children(func() {
		ui.Row(c).Width(float32(10 + 16*depth)).Shrink(0).Children(func() {})
		brandIcon(c, fileTypeIcon(file.Path), 13)
		name := file.Path
		if index := strings.LastIndex(file.Path, "/"); index >= 0 {
			name = file.Path[index+1:]
		}
		ui.Text(c, name).FontSize(uiPx(11.5)).TextColor(palette.textSecondary).Grow(1).MinWidth(0).SingleLine().Ellipsis("...")
		ui.Text(c, file.Letter).FontSize(uiPx(10.5)).TextColor(scLetterColor(file.Letter))
		if st.writable() {
			f := file
			stageLabel := tr("Stage")
			if f.Group == "staged" {
				stageLabel = tr("Unstage")
			}
			stage := reviewMutedButton(c, stageLabel)
			stage.OnClick(func() { a.scStage(f, f.Group == "staged") })
			if f.Group != "staged" {
				discard := reviewMutedButton(c, tr("Discard…"))
				discard.OnClick(func() { st.Discard = &f })
			}
		}
	})
	entry.OnClick(func() {
		if st.Selected != file.key() {
			st.Selected = file.key()
			for i, row := range st.Rows {
				if row.key() == file.key() {
					st.FileIndex = i
					break
				}
			}
			st.Reader.Context = 3
			a.scLoadDiff()
		}
	})
}

func (a *app) scDiscardDialog(c *ui.Context) {
	st := &a.sourceControl
	file := *st.Discard
	ui.Column(c).FillWidth().Padding(10, 12).Gap(6).Background(palette.raised).Children(func() {
		text := tr("Discard unstaged changes to ") + file.Path + tr("?")
		if file.Group == "untracked" {
			text = tr("Permanently delete ") + file.Path + tr("? This cannot be undone.")
		}
		reviewNote(c, text, true)
		ui.Row(c).FillWidth().Gap(6).Children(func() {
			c.Key("sc-confirm-discard")
			confirm := action.Button(c, tr("Confirm discard"), action.ButtonProps{Variant: action.Default, Size: theme.Small})
			if !st.writable() {
				confirm.Disabled(true)
			} else {
				confirm.OnClick(a.scConfirmDiscard)
			}
			cancel := action.Button(c, tr("Cancel"), action.ButtonProps{Variant: action.Text, Size: theme.Small})
			cancel.OnClick(func() { st.Discard = nil })
		})
		c.OnShortcut(0, ui.KeyEscape, func() { st.Discard = nil })
	})
}

func (a *app) scMain(c *ui.Context) {
	main := ui.Column(c).Grow(1).MinWidth(0).FillHeight().Background(palette.surface).
		BorderWidth(0, 0, 0, 1).BorderColor(palette.sidebarBorder).Radius(workspaceRadius, 0, 0, workspaceRadius).Clip()
	main = workspaceShadow(main)
	main.Children(func() {
		a.scToolbar(c)
		st := &a.sourceControl
		if st.Error != "" {
			reviewEmpty(c, st.Error, true)
			return
		}
		if file := st.selectedFile(); file != nil {
			a.reviewReaderView(c, file.Path, &st.Reader, false)
			return
		}
		reviewEmpty(c, tr("Select a file to review"), false)
	})
}

// scToolbar mirrors render_review_diff_toolbar for the live area: source
// pill dropdown, truncated flag, spacer, file position, prev/next file,
// refresh.
func (a *app) scToolbar(c *ui.Context) {
	st := &a.sourceControl
	total := len(st.Rows)
	current := st.FileIndex + 1
	if total == 0 {
		current = 0
	}
	ui.Row(c).FillWidth().Height(44).Padding(0, 12).Gap(8).AlignItems(ui.Center).
		BorderWidth(0, 0, 1, 0).BorderColor(palette.border).Children(func() {
		a.reviewSourcePill(c, false)
		if st.Stats.Truncated {
			ui.Text(c, tr("Truncated")).FontSize(uiPx(10.5)).TextColor(palette.warningText)
		}
		ui.Row(c).Grow(1).Children(func() {})
		a.reviewFileNav(c, total, current, a.scStepFile)
		a.reviewRefreshButton(c, false)
	})
}

func scLetterColor(letter string) ui.Color {
	switch letter {
	case "A", "C":
		return palette.successText
	case "D":
		return palette.dangerText
	case "M", "R", "T", "U":
		return palette.warningText
	default:
		return palette.textTertiary
	}
}
