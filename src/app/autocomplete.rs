//! The composer's autocompletion popup: slash commands and `@` skill/file mentions.
//!
//! The popup is a pure view over prefetched data. Command and file indexes are
//! discovered on the background executor into `QueryCache`s and mirrored into
//! plain fields the frame reads; the filter over them is memoized per
//! keystroke, so a caret blink re-renders the popup without re-fuzzy-matching
//! the workspace.
//!
//! Keys follow the model picker's split: the composer keeps real focus the
//! whole time and the popup's selection is only drawn. While the popup is
//! open the composer card declares the `ComposerAutocomplete` key context, and
//! `up`/`down`/`enter`/`tab`/`escape` reach it as actions that outrank the
//! field's own bindings; when it closes the context disappears and `enter`
//! submits again.

use crate::theme::ui_px;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Anchor, App, Bounds, Font, KeyBinding, Pixels, StyledText, TextRun, actions, anchored, deferred,
};
use nucleo_matcher::Matcher;

use crate::composer_complete::{
    self, FILE_INDEX_CAP, FileEntry, Scored, SlashCommand, Trigger, TriggerKind,
    highlight_byte_ranges,
};
use crate::persistence::ComposerRecentEntry;
use crate::ui::menu::{ConfirmEntry, DismissMenu, SelectNextEntry, SelectPreviousEntry};

use super::composer::next_picker_highlight;
use super::*;

/// Key context the composer card declares while the popup is open.
const AUTOCOMPLETE_CONTEXT: &str = "ComposerAutocomplete > ComposerInput";
const AUTOCOMPLETE_BROWSE_CONTEXT: &str = "ComposerAutocompleteBrowse > ComposerInput";
const RECENT_LIMIT: usize = 12;

actions!(fintwind_autocomplete, [PreviousCategory, NextCategory]);

/// Bind the popup's keys. Must run after [`crate::input::init`]: `enter` and
/// the arrows tie with the field's own bindings at the `ComposerInput` depth,
/// and the tie goes to whichever was registered last.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNextEntry, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("up", SelectPreviousEntry, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("enter", ConfirmEntry, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("tab", ConfirmEntry, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("escape", DismissMenu, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("left", PreviousCategory, Some(AUTOCOMPLETE_BROWSE_CONTEXT)),
        KeyBinding::new("right", NextCategory, Some(AUTOCOMPLETE_BROWSE_CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Category {
    #[default]
    Recent,
    Commands,
    Skills,
    Files,
}

impl Category {
    fn for_trigger(kind: TriggerKind) -> [Self; 3] {
        match kind {
            TriggerKind::Command => [Self::Recent, Self::Commands, Self::Skills],
            TriggerKind::File => [Self::Recent, Self::Skills, Self::Files],
        }
    }

    fn label(self) -> String {
        match self {
            Self::Recent => tr!("composer_completion.recent"),
            Self::Commands => tr!("composer_completion.commands"),
            Self::Skills => tr!("composer_completion.skills"),
            Self::Files => tr!("composer_completion.files"),
        }
    }
}

pub(super) enum AutocompleteRow {
    Command(Scored<SlashCommand>),
    Skill(Scored<SlashCommand>),
    File(Scored<FileEntry>),
}

/// Filter results for one (kind, query, source index) — the popup's rows are
/// recomputed on a keystroke, not on every frame the caret blinks.
struct ResultsMemo {
    kind: TriggerKind,
    query: String,
    /// `Rc::as_ptr` identity of the source index the rows were filtered from.
    source: (usize, usize),
    category: Category,
    project: Option<Uuid>,
    rows: Rc<Vec<AutocompleteRow>>,
}

/// Cross-frame state for the popup. All interior-mutable: the render path
/// reconciles it from `&self`, the same way the transcript anchors do.
pub(super) struct AutocompleteUi {
    /// The trigger as of the last frame, for detecting query/site changes.
    token: RefCell<Option<Trigger>>,
    /// Keyboard cursor over the filtered rows: the popup opens with the first
    /// row selected, and every token change snaps back to it so the best
    /// match is always the one `enter` takes. Clamped to the list at use.
    highlight: Cell<usize>,
    category: Cell<Category>,
    /// Escape pressed on the current token; cleared the moment it changes.
    dismissed: Cell<bool>,
    scroll: ScrollHandle,
    /// The composer card's bounds as of the last frame, recorded by a probe,
    /// so the popup can anchor above the card at the card's own width.
    card_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    results: RefCell<Option<ResultsMemo>>,
    matcher: RefCell<Matcher>,
}

impl AutocompleteUi {
    pub(super) fn new() -> Self {
        Self {
            token: RefCell::new(None),
            highlight: Cell::new(0),
            category: Cell::new(Category::Recent),
            dismissed: Cell::new(false),
            scroll: ScrollHandle::new(),
            card_bounds: Rc::new(Cell::new(None)),
            results: RefCell::new(None),
            matcher: RefCell::new(composer_complete::matcher()),
        }
    }

    /// The cell the composer card's bounds probe writes into.
    pub(super) fn card_bounds_cell(&self) -> Rc<Cell<Option<Bounds<Pixels>>>> {
        self.card_bounds.clone()
    }

    pub(super) fn browsing(&self) -> bool {
        self.token
            .borrow()
            .as_ref()
            .is_some_and(|token| token.query.is_empty())
    }

    fn reset_selection(&self) {
        self.highlight.set(0);
        self.scroll.scroll_to_item(0);
    }
}

impl Fintwind {
    /// Refresh the drawn command and file indexes for the selected session.
    ///
    /// A cache hit lands immediately; a miss starts discovery on the
    /// background executor and re-runs this when it arrives. Nothing here may
    /// touch the filesystem directly.
    pub(super) fn refresh_composer_sources(&mut self, cx: &mut Context<Self>) {
        let Some(project_path) = self
            .selected_workspace_path()
            .map(std::path::Path::to_path_buf)
        else {
            self.slash_command_index = Rc::new(self.composer_goal_commands(&[], &[]));
            self.slash_command_index_key = None;
            self.mention_file_index = Rc::new(Vec::new());
            self.mention_file_index_path = None;
            self.mention_file_index_unavailable = true;
            self.sync_composer_reference_catalog(cx);
            return;
        };
        let provider = self
            .selected_session()
            .map(|session| session.provider.clone())
            .unwrap_or_else(|| self.state.last_provider.clone());
        let reported = self
            .selected_session()
            .map(|session| session.available_commands.clone())
            .unwrap_or_default();

        let command_key = (provider, project_path.clone());
        match self.slash_commands.read(&command_key) {
            Query::Ready(commands) => {
                self.slash_command_index =
                    Rc::new(self.composer_goal_commands(&commands, &reported));
                self.slash_command_index_key = Some(command_key);
            }
            Query::Pending => {
                // A scan for this exact key is in flight; anything drawn
                // meanwhile must not be another provider's list.
                if self.slash_command_index_key.as_ref() != Some(&command_key) {
                    self.slash_command_index = Rc::new(self.composer_goal_commands(&[], &reported));
                    self.slash_command_index_key = None;
                }
            }
            Query::Missing(token) => {
                if self.slash_command_index_key.as_ref() != Some(&command_key) {
                    self.slash_command_index = Rc::new(self.composer_goal_commands(&[], &reported));
                    self.slash_command_index_key = None;
                }
                let path = project_path.clone();
                let binary = self.probes.first().and_then(|probe| probe.path.clone());
                let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
                cx.spawn(async move |fintwind, cx| {
                    let commands = cx
                        .background_executor()
                        .spawn(async move {
                            match workspace.request(
                                fintwind_client::WorkspaceOperation::DiscoverSlashCommands {
                                    project_root: path,
                                    binary,
                                },
                            ) {
                                Ok(fintwind_client::WorkspaceResult::SlashCommands {
                                    commands,
                                }) => commands,
                                Ok(_) | Err(_) => Vec::new(),
                            }
                        })
                        .await;
                    fintwind
                        .update(cx, |fintwind, cx| {
                            if fintwind.slash_commands.fulfill(token, commands) {
                                fintwind.refresh_composer_sources(cx);
                                cx.notify();
                            }
                        })
                        .ok();
                })
                .detach();
            }
        }

        match self.mention_files.read(&project_path) {
            Query::Ready(files) => {
                self.mention_file_index_unavailable = files.is_none();
                self.mention_file_index = files.as_ref().clone().unwrap_or_default().into();
                self.mention_file_index_path = Some(project_path);
            }
            Query::Pending => {
                if self.mention_file_index_path.as_ref() != Some(&project_path) {
                    self.mention_file_index = Rc::new(Vec::new());
                    self.mention_file_index_path = None;
                    self.mention_file_index_unavailable = false;
                }
            }
            Query::Missing(token) => {
                if self.mention_file_index_path.as_ref() != Some(&project_path) {
                    self.mention_file_index = Rc::new(Vec::new());
                    self.mention_file_index_path = None;
                    self.mention_file_index_unavailable = false;
                }
                let path = project_path.clone();
                let workspace = fintwind_client::WorkspaceClient::new(self.daemon.client());
                cx.spawn(async move |fintwind, cx| {
                    let files = cx
                        .background_executor()
                        .spawn(async move {
                            match workspace.request(
                                fintwind_client::WorkspaceOperation::ListProjectFiles {
                                    root: path,
                                    cap: FILE_INDEX_CAP,
                                },
                            ) {
                                Ok(fintwind_client::WorkspaceResult::ProjectFiles { entries }) => {
                                    Some(entries)
                                }
                                Ok(_) | Err(_) => None,
                            }
                        })
                        .await;
                    fintwind
                        .update(cx, |fintwind, cx| {
                            if fintwind.mention_files.fulfill(token, files) {
                                fintwind.refresh_composer_sources(cx);
                                cx.notify();
                            }
                        })
                        .ok();
                })
                .detach();
            }
        }
        self.sync_composer_reference_catalog(cx);
    }

    fn sync_composer_reference_catalog(&mut self, cx: &mut Context<Self>) {
        if self
            .composer_reference_catalog
            .has_sources(&self.slash_command_index, &self.mention_file_index)
        {
            return;
        }
        let catalog = Rc::new(crate::mentions::Catalog::new(
            self.slash_command_index.clone(),
            self.mention_file_index.clone(),
            super::right_panel::file_icon_for_path,
        ));
        self.composer_reference_catalog = catalog.clone();
        self.composer.update(cx, |input, cx| {
            input.set_reference_catalog(catalog.clone(), cx)
        });
        if let Some(edit) = &self.message_edit {
            edit.input
                .update(cx, |input, cx| input.set_reference_catalog(catalog, cx));
        }
    }

    /// Invalidate and re-request both indexes for the selected workspace.
    pub(super) fn invalidate_composer_sources(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self
            .selected_workspace_path()
            .map(std::path::Path::to_path_buf)
        {
            let provider = self
                .selected_session()
                .map(|session| session.provider.clone())
                .unwrap_or_else(|| self.state.last_provider.clone());
            self.slash_commands.invalidate(&(provider, path.clone()));
            self.mention_files.invalidate(&path);
        }
        self.refresh_composer_sources(cx);
    }

    /// The trigger under the composer's caret, reconciled with the popup's
    /// cross-frame state. `None` while the composer is unfocused, the token is
    /// dismissed, or there is nothing to complete.
    fn composer_trigger(&self, window: &Window, cx: &App) -> Option<Trigger> {
        let input = self.composer.read(cx);
        let trigger = if input.focus().is_focused(window) {
            composer_complete::detect_trigger(input.content(), input.cursor())
        } else {
            None
        };
        let ui = &self.composer_autocomplete;
        if *ui.token.borrow() != trigger {
            if trigger.as_ref().is_some_and(|token| token.query.is_empty()) {
                ui.category.set(Category::Recent);
            }
            *ui.token.borrow_mut() = trigger.clone();
            // A different token renumbers the rows: the keyboard cursor and a
            // standing dismissal both describe the previous list.
            ui.reset_selection();
            ui.dismissed.set(false);
        }
        if ui.dismissed.get() {
            return None;
        }
        trigger
    }

    fn autocomplete_project(&self) -> Option<Uuid> {
        self.selected_session()
            .map(|session| session.project_id)
            .or(self.state.selected_project)
    }

    fn autocomplete_recent_rows(
        &self,
        project: Option<Uuid>,
        kind: TriggerKind,
    ) -> Vec<AutocompleteRow> {
        let Some(recent) = project.and_then(|id| self.state.composer_recent.get(&id)) else {
            return Vec::new();
        };
        let entries = match kind {
            TriggerKind::Command => &recent.slash,
            TriggerKind::File => &recent.mention,
        };
        entries
            .iter()
            .filter_map(|entry| match entry {
                ComposerRecentEntry::Command(name) | ComposerRecentEntry::Skill(name) => {
                    let is_skill = matches!(entry, ComposerRecentEntry::Skill(_));
                    self.slash_command_index
                        .iter()
                        .find(|command| {
                            command.name == *name
                                && is_skill
                                    == (command.scope == composer_complete::CommandScope::Skill)
                        })
                        .map(|item| {
                            let scored = Scored {
                                item: item.clone(),
                                positions: Vec::new(),
                            };
                            if is_skill {
                                AutocompleteRow::Skill(scored)
                            } else {
                                AutocompleteRow::Command(scored)
                            }
                        })
                }
                ComposerRecentEntry::File(path) => self
                    .mention_file_index
                    .iter()
                    .find(|file| file.path == *path)
                    .map(|file| {
                        AutocompleteRow::File(Scored {
                            item: file.clone(),
                            positions: Vec::new(),
                        })
                    }),
            })
            .collect()
    }

    /// The filtered rows for `trigger`, shared by the popup body, the keyboard
    /// cursor and `enter` so an index always means the same row everywhere.
    fn autocomplete_rows(&self, trigger: &Trigger) -> Rc<Vec<AutocompleteRow>> {
        let browsing = trigger.query.is_empty();
        let category = self.composer_autocomplete.category.get();
        let project = self.autocomplete_project();
        let source = match trigger.kind {
            TriggerKind::Command => (Rc::as_ptr(&self.slash_command_index) as usize, 0),
            TriggerKind::File => (
                Rc::as_ptr(&self.mention_file_index) as usize,
                Rc::as_ptr(&self.slash_command_index) as usize,
            ),
        };
        {
            let memo = self.composer_autocomplete.results.borrow();
            if let Some(memo) = memo.as_ref().filter(|memo| {
                memo.kind == trigger.kind
                    && memo.query == trigger.query
                    && memo.source == source
                    && memo.category == category
                    && memo.project == project
            }) {
                return memo.rows.clone();
            }
        }
        let mut matcher = self.composer_autocomplete.matcher.borrow_mut();
        let is_skill =
            |command: &SlashCommand| command.scope == composer_complete::CommandScope::Skill;
        let rows = match (browsing, category, trigger.kind) {
            (true, Category::Recent, kind) => self.autocomplete_recent_rows(project, kind),
            (true, Category::Files, _) => {
                composer_complete::filter_files(&self.mention_file_index, "", &mut matcher)
                    .into_iter()
                    .map(AutocompleteRow::File)
                    .collect()
            }
            (false, _, TriggerKind::File) => {
                let skills = self
                    .slash_command_index
                    .iter()
                    .filter(|command| is_skill(command))
                    .cloned()
                    .collect::<Vec<_>>();
                let files = composer_complete::filter_files(
                    &self.mention_file_index,
                    &trigger.query,
                    &mut matcher,
                );
                // Reserve room for both kinds even when the skill catalog is large.
                let skill_limit = composer_complete::FILTER_CAP
                    - files.len().min(composer_complete::FILTER_CAP / 2);
                let mut rows =
                    composer_complete::filter_commands(&skills, &trigger.query, &mut matcher)
                        .into_iter()
                        .take(skill_limit)
                        .map(AutocompleteRow::Skill)
                        .collect::<Vec<_>>();
                let remaining = composer_complete::FILTER_CAP - rows.len();
                rows.extend(files.into_iter().take(remaining).map(AutocompleteRow::File));
                rows
            }
            (true, category, _) => {
                let entries = self
                    .slash_command_index
                    .iter()
                    .filter(|command| is_skill(command) == (category == Category::Skills))
                    .cloned()
                    .collect::<Vec<_>>();
                composer_complete::filter_commands(&entries, "", &mut matcher)
                    .into_iter()
                    .map(|scored| {
                        if is_skill(&scored.item) {
                            AutocompleteRow::Skill(scored)
                        } else {
                            AutocompleteRow::Command(scored)
                        }
                    })
                    .collect()
            }
            (false, _, TriggerKind::Command) => composer_complete::filter_commands(
                &self.slash_command_index,
                &trigger.query,
                &mut matcher,
            )
            .into_iter()
            .map(|scored| {
                if is_skill(&scored.item) {
                    AutocompleteRow::Skill(scored)
                } else {
                    AutocompleteRow::Command(scored)
                }
            })
            .collect(),
        };
        let rows = Rc::new(rows);
        *self.composer_autocomplete.results.borrow_mut() = Some(ResultsMemo {
            kind: trigger.kind,
            query: trigger.query.clone(),
            source,
            category,
            project,
            rows: rows.clone(),
        });
        rows
    }

    fn set_autocomplete_category(&self, category: Category, cx: &mut Context<Self>) {
        self.composer_autocomplete.category.set(category);
        self.composer_autocomplete.reset_selection();
        cx.notify();
    }

    pub(super) fn cycle_autocomplete_category(
        &self,
        forward: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(trigger) = self
            .composer_trigger(window, cx)
            .filter(|trigger| trigger.query.is_empty())
        else {
            return;
        };
        let categories = Category::for_trigger(trigger.kind);
        let index = categories
            .iter()
            .position(|category| *category == self.composer_autocomplete.category.get())
            .unwrap_or(0);
        let step = if forward { 1 } else { categories.len() - 1 };
        self.set_autocomplete_category(categories[(index + step) % categories.len()], cx);
    }

    pub(super) fn move_autocomplete_highlight(
        &mut self,
        key: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(trigger) = self.composer_trigger(window, cx) else {
            return;
        };
        let rows = self.autocomplete_rows(&trigger);
        let ui = &self.composer_autocomplete;
        let current = ui.highlight.get().min(rows.len().saturating_sub(1));
        let Some(next) = next_picker_highlight(Some(current), rows.len(), key) else {
            return;
        };
        ui.highlight.set(next);
        ui.scroll.scroll_to_item(next);
        cx.notify();
    }

    /// Insert the chosen row over the trigger token. `index` comes from a
    /// click; `None` is the keyboard path, which takes the drawn cursor and
    /// defaults to the first row so `enter` works the moment the popup opens.
    pub(super) fn accept_autocomplete(
        &mut self,
        index: Option<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(trigger) = self.composer_trigger(window, cx) else {
            return;
        };
        let rows = self.autocomplete_rows(&trigger);
        let index = index.unwrap_or_else(|| {
            self.composer_autocomplete
                .highlight
                .get()
                .min(rows.len().saturating_sub(1))
        });
        let Some(row) = rows.get(index) else {
            return;
        };
        let (name, entry) = match row {
            AutocompleteRow::Command(scored) => (
                &scored.item.name,
                ComposerRecentEntry::Command(scored.item.name.clone()),
            ),
            AutocompleteRow::Skill(scored) => (
                &scored.item.name,
                ComposerRecentEntry::Skill(scored.item.name.clone()),
            ),
            AutocompleteRow::File(scored) => (
                &scored.item.path,
                ComposerRecentEntry::File(scored.item.path.clone()),
            ),
        };
        let sigil = if trigger.kind == TriggerKind::Command {
            '/'
        } else {
            '@'
        };
        let insert = format!("{sigil}{name} ");
        self.composer.update(cx, |input, cx| {
            input.replace_range(trigger.range.clone(), &insert, cx);
        });
        if let Some(project) = self.autocomplete_project() {
            let recent = self.state.composer_recent.entry(project).or_default();
            let entries = match trigger.kind {
                TriggerKind::Command => &mut recent.slash,
                TriggerKind::File => &mut recent.mention,
            };
            if entries.first() != Some(&entry) {
                entries.retain(|previous| *previous != entry);
                entries.insert(0, entry);
                entries.truncate(RECENT_LIMIT);
                self.composer_autocomplete.results.borrow_mut().take();
                self.save();
            }
        }
        cx.notify();
    }

    pub(super) fn dismiss_autocomplete(&mut self, cx: &mut Context<Self>) {
        self.composer_autocomplete.dismissed.set(true);
        cx.notify();
    }

    /// The popup, anchored above the composer card, or `None` when idle.
    ///
    /// Reads only the prefetched indexes — discovery never runs on a frame.
    pub(super) fn render_composer_autocomplete(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let trigger = self.composer_trigger(window, cx)?;
        let rows = self.autocomplete_rows(&trigger);
        // The probe records during paint, so the first frame a composer ever
        // draws has no bounds yet; the popup appears one frame later.
        let card_bounds = self.composer_autocomplete.card_bounds.get()?;
        let theme = Theme::current(cx);
        let browsing = trigger.query.is_empty();
        let category = self.composer_autocomplete.category.get();
        let highlight = self
            .composer_autocomplete
            .highlight
            .get()
            .min(rows.len().saturating_sub(1));

        let tabs = browsing.then(|| {
            div().flex().gap(px(4.0)).px(px(4.0)).pt(px(6.0)).flex_none().children(
                Category::for_trigger(trigger.kind)
                    .into_iter()
                    .enumerate()
                    .map(|(index, tab)| {
                        let selected = category == tab;
                        div()
                            .id(("autocomplete-category", index))
                            .h(ui_px(26.0))
                            .px(px(8.0))
                            .rounded(px(5.0))
                            .flex()
                            .items_center()
                            .text_size(ui_px(11.5))
                            .text_color(theme.text_secondary)
                            .hover(|tab| tab.bg(theme.overlay))
                            .when(selected, |tab| {
                                tab.bg(theme.overlay_strong)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                            })
                            .child(tab.label())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.set_autocomplete_category(tab, cx);
                                }),
                            )
                    }),
            )
        });
        let mut list = div()
            .id("composer-autocomplete-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.composer_autocomplete.scroll)
            .p(px(4.0))
            .when(rows.is_empty(), |list| {
                let label = if browsing && category == Category::Recent {
                    tr!("composer_completion.empty_recent")
                } else if trigger.kind == TriggerKind::File
                    && (!browsing || category == Category::Files)
                    && self.mention_file_index_unavailable
                {
                    tr!("composer_completion.files_unavailable")
                } else if trigger.kind == TriggerKind::File
                    && (!browsing || category == Category::Files)
                    && self.mention_file_index_path.is_none()
                {
                    tr!("composer_completion.files_loading")
                } else {
                    tr!("composer_completion.no_matches")
                };
                list.child(
                    div()
                        .h(px(116.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.text_tertiary)
                        .text_size(ui_px(12.0))
                        .child(label),
                )
            });
        for (index, row) in rows.iter().enumerate() {
            list = list.child(self.render_autocomplete_row(
                index,
                row,
                highlight,
                trigger.kind,
                &theme,
                window,
                cx,
            ));
        }
        let footer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .px(px(8.0))
            .h(ui_px(26.0))
            .border_t_1()
            .border_color(theme.border)
            .text_size(ui_px(10.0))
            .text_color(theme.text_tertiary)
            .when(!rows.is_empty(), |bar| {
                bar.child(tr!("composer_completion.select"))
            })
            .when(browsing, |bar| bar.child(tr!("composer_completion.switch")))
            .when(!rows.is_empty(), |bar| {
                bar.child(tr!("composer_completion.insert"))
            })
            .child(div().flex_1())
            .child(tr!("composer_completion.dismiss"));

        Some(
            deferred(
                anchored()
                    .position(point(card_bounds.origin.x, card_bounds.origin.y - px(6.0)))
                    .anchor(Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .occlude()
                            .w(card_bounds.size.width.min(px(560.0)))
                            .max_h((card_bounds.origin.y - px(14.0)).clamp(px(96.0), px(416.0)))
                            .rounded(px(11.0))
                            .border_1()
                            .border_color(theme.border_strong)
                            .bg(theme.raised)
                            .shadow_lg()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.dismiss_autocomplete(cx);
                            }))
                            .children(tabs)
                            .child(list)
                            .child(footer),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn render_autocomplete_row(
        &self,
        index: usize,
        row: &AutocompleteRow,
        highlight: usize,
        kind: TriggerKind,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let highlighted = highlight == index;
        let font = window.text_style().font();
        let base = div()
            .id(index)
            .h(px(30.0))
            .px(px(8.0))
            // Concentric with the popup card: 11px shell minus the 4px list
            // padding.
            .rounded(px(7.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_default()
            .when(highlighted, |element| element.bg(theme.overlay_strong))
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.accept_autocomplete(Some(index), window, cx);
                }),
            );
        match row {
            AutocompleteRow::Command(scored) | AutocompleteRow::Skill(scored) => {
                let command = &scored.item;
                let sigil = if kind == TriggerKind::Command {
                    '/'
                } else {
                    '@'
                };
                let icon_path = if command.scope == composer_complete::CommandScope::Skill {
                    crate::mentions::SKILL_ICON
                } else {
                    "icons/command.svg"
                };
                // Positions index the bare name; the drawn `/` shifts every
                // byte range right by one.
                let name_ranges = highlight_byte_ranges(&command.name, &scored.positions, 0)
                    .into_iter()
                    .map(|range| range.start + 1..range.end + 1)
                    .collect();
                let mut name_font = font.clone();
                name_font.weight = FontWeight::MEDIUM;
                base.child(icon(icon_path, 12.0, theme.text_tertiary))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(260.0))
                            .truncate()
                            .text_size(ui_px(12.0))
                            .child(matched_text(
                                format!("{sigil}{}", command.name),
                                name_ranges,
                                theme.text,
                                theme.accent,
                                name_font,
                            )),
                    )
                    .when_some(command.argument_hint.clone(), |element, hint| {
                        element.child(
                            div()
                                .flex_none()
                                .text_size(ui_px(11.0))
                                .text_color(theme.text_muted)
                                .child(hint),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(11.0))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(command.description.clone())),
                    )
                    .child(
                        div()
                            .h(ui_px(16.0))
                            .px(px(5.0))
                            .flex_none()
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .text_size(ui_px(9.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_tertiary)
                            .child(command.scope.label()),
                    )
                    .into_any_element()
            }
            AutocompleteRow::File(scored) => {
                let file = &scored.item;
                // The row draws basename then directory, but the positions
                // index the full path — each segment recovers its own ranges.
                // A directory's trailing slash stays with the basename, so a
                // match on it still paints.
                let trimmed_len = file.path.trim_end_matches('/').len();
                let name_start = file.path[..trimmed_len]
                    .rfind('/')
                    .map_or(0, |index| index + 1);
                let name = &file.path[name_start..];
                let parent = &file.path[..name_start.saturating_sub(1)];
                let name_char_offset = file.path[..name_start].chars().count();
                base.when(!file.is_dir, |element| {
                    element.child(crate::ui::file_icon(
                        super::right_panel::file_icon_for_path(&file.path),
                        13.0,
                    ))
                })
                .child(
                    div()
                        .flex_none()
                        .max_w(px(300.0))
                        .truncate()
                        .text_size(ui_px(12.0))
                        .child(matched_text(
                            name.to_owned(),
                            highlight_byte_ranges(name, &scored.positions, name_char_offset),
                            theme.text,
                            theme.accent,
                            font.clone(),
                        )),
                )
                .when(!parent.is_empty(), |element| {
                    element.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(11.0))
                            .child(matched_text(
                                parent.to_owned(),
                                highlight_byte_ranges(parent, &scored.positions, 0),
                                theme.text_ghost,
                                theme.accent,
                                font,
                            )),
                    )
                })
                .into_any_element()
            }
        }
    }
}

/// Text with the fuzzy-matched byte ranges lifted to the accent colour and a
/// semibold weight, the runs tiling the string exactly. `ranges` are sorted
/// and non-overlapping, as [`highlight_byte_ranges`] returns them.
fn matched_text(
    text: String,
    ranges: Vec<std::ops::Range<usize>>,
    base_color: Hsla,
    accent: Hsla,
    font: Font,
) -> StyledText {
    // A step above either base weight in these rows — regular file paths and
    // medium command names both read as "a bit bolder", not shouting.
    let mut accent_font = font.clone();
    accent_font.weight = FontWeight::SEMIBOLD;
    let run = |len: usize, font: Font, color: Hsla| TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut cursor = 0;
    for range in ranges {
        if range.start > cursor {
            runs.push(run(range.start - cursor, font.clone(), base_color));
        }
        runs.push(run(range.len(), accent_font.clone(), accent));
        cursor = range.end;
    }
    if cursor < text.len() {
        runs.push(run(text.len() - cursor, font.clone(), base_color));
    }
    StyledText::new(text).with_runs(runs)
}

/// The probe recording the composer card's bounds for the popup's anchor.
/// `inset_0` so it reports the border box, not the padded content box.
pub(super) fn composer_card_bounds_probe(
    cell: Rc<Cell<Option<Bounds<Pixels>>>>,
) -> impl IntoElement {
    canvas(
        move |bounds: Bounds<Pixels>, _, _| cell.set(Some(bounds)),
        |_, _, _, _| (),
    )
    .absolute()
    .inset_0()
}
