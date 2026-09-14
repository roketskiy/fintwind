//! The Providers settings page: the model-provider roster as a mail-style
//! master–detail split — the provider list on the left, the selected
//! provider's endpoint, key, and models on the right, or the add-provider
//! form in the detail's place.
//!
//! The roster is OpenCode's own configuration file
//! (`~/.config/opencode/opencode.json`): it is loaded from there when the
//! page (or the app) opens and every mutation commits straight back, so
//! entries created with the CLI, the TUI, or an editor are the same data.
//! OpenCode hot-reloads the file, and a commit re-probes so the composer's
//! picker picks up the new catalog. The built-in OpenCode provider stays
//! read-only: its endpoint, key, and catalog belong to the CLI (see
//! `fintwind_client::opencode_config`).
//!
//! Field edits debounce their commit; discrete actions (add, delete, toggle,
//! rename) commit immediately, the same one-shot-action allowance the Skills
//! page uses.

use crate::theme::ui_px;

use gpui::{ElementId, KeyDownEvent};

use fintwind_client::custom_providers::{self, CustomProvider, CustomProviderModel, ProviderApiFormat};

use super::*;

/// #D97757 — this page's selection and accent color. The dark appearance
/// uses it as authored; light darkens it, the way `Theme::accent` is tuned
/// per appearance, so fills, dots, and badge text keep their contrast on
/// light surfaces.
fn providers_accent(theme: &Theme) -> Hsla {
    if theme.is_dark {
        rgb(0xD97757).into()
    } else {
        rgb(0xB25A39).into()
    }
}

/// Glyph color on an accent fill. The dark appearance's #D97757 is a
/// mid-tone, so near-black glyphs out-read white ones there; the light
/// appearance darkens the fill instead and takes white.
fn on_providers_accent(theme: &Theme) -> Hsla {
    if theme.is_dark {
        rgb(0x201814).into()
    } else {
        rgb(0xFFFFFF).into()
    }
}

const PROVIDERS_LIST_WIDTH: f32 = 264.0;

/// Which detail field a keystroke edit landed in.
#[derive(Clone, Copy)]
pub(super) enum ProviderField {
    BaseUrl,
    ApiKey,
}

/// Which model row the inline model editor is working on.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ProvidersModelEditor {
    Add,
    Edit(usize),
}

/// One model draft row of the add-provider form: the id and context-window
/// fields plus the input modalities picked for the model. A draft exists
/// before its model does, so the selection lives here rather than on a
/// [`CustomProviderModel`].
pub(super) struct ProviderFormModelDraft {
    pub(super) id: Entity<ComposerInput>,
    pub(super) context: Entity<ComposerInput>,
    pub(super) input_modalities: Vec<String>,
}

pub(super) fn api_format_label(format: ProviderApiFormat) -> String {
    match format {
        ProviderApiFormat::OpenAi => tr!("providers.api_format_openai"),
        ProviderApiFormat::OpenAiResponses => tr!("providers.api_format_openai_responses"),
        ProviderApiFormat::Anthropic => tr!("providers.api_format_anthropic"),
    }
}

impl Fintwind {
    // ── Selection & state ──────────────────────────────────────────────────

    /// The provider id the detail pane shows: the stored selection while it
    /// still resolves, the built-in provider otherwise.
    fn effective_provider_id(&self) -> String {
        match self.providers_selected.as_deref() {
            Some(id) if id == OPENCODE_PROVIDER => id.to_owned(),
            Some(id)
                if self
                    .providers_store
                    .iter()
                    .any(|provider| provider.id == id) =>
            {
                id.to_owned()
            }
            _ => OPENCODE_PROVIDER.to_owned(),
        }
    }

    fn select_provider(&mut self, id: String, cx: &mut Context<Self>) {
        self.providers_selected = Some(id.clone());
        self.exit_provider_form(cx);
        self.providers_renaming = false;
        self.providers_api_key_revealed = false;
        self.providers_delete_arming = None;
        self.providers_model_editor = None;
        self.providers_model_editor_modalities.clear();
        self.load_provider_fields(&id, cx);
        self.providers_detail_scroll
            .set_offset(gpui::Point::default());
        cx.notify();
    }

    /// Load the detail fields from the provider now under selection. The
    /// fields are the only editors of these values, so this runs on selection
    /// changes; the `Edited` handler's echo guard skips the reflection.
    fn load_provider_fields(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(provider) = self
            .providers_store
            .iter()
            .find(|provider| provider.id == id)
            .cloned()
        else {
            return;
        };
        self.provider_base_url_input.update(cx, |input, cx| {
            if input.content() != provider.base_url {
                input.set_content(provider.base_url.clone(), cx);
            }
        });
        self.provider_api_key_input.update(cx, |input, cx| {
            if input.content() != provider.api_key {
                input.set_content(provider.api_key.clone(), cx);
            }
        });
    }

    /// Clear the page's transient editor state and re-read OpenCode's
    /// configuration, so a provider added or edited with the CLI since the
    /// last visit is already on the roster. Called when the page opens; a
    /// half-finished rename or model edit never survives the visit.
    pub(super) fn reset_providers_page(&mut self, cx: &mut Context<Self>) {
        self.exit_provider_form(cx);
        self.providers_renaming = false;
        self.providers_api_key_revealed = false;
        self.providers_delete_arming = None;
        self.providers_model_editor = None;
        self.providers_model_editor_modalities.clear();
        self.providers_form_models.clear();
        if let Some(id) = self.providers_selected.clone() {
            self.load_provider_fields(&id, cx);
        }
        self.load_providers_from_config(cx);
        // The modality badges need the metadata table; load it for the visit.
        self.ensure_models_dev_table(cx);
    }

    // ── Persistence ────────────────────────────────────────────────────────

    /// Load the roster from OpenCode's own configuration off-thread, after
    /// migrating the old app-managed mirror file once. The result replaces
    /// the working store unless a newer load started or a debounced field
    /// edit is pending.
    pub(super) fn load_providers_from_config(&mut self, cx: &mut Context<Self>) {
        self.providers_load_generation += 1;
        let generation = self.providers_load_generation;
        cx.spawn(async move |this, cx| {
            let migrated = cx
                .background_executor()
                .spawn(async move {
                    fintwind_client::opencode_config::migrate_legacy_override_file();
                    fintwind_client::opencode_config::load_providers()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.providers_load_generation != generation
                    || this.providers_commit_generation != 0
                {
                    return;
                }
                match migrated {
                    Ok(providers) => {
                        let selected_missing =
                            this.providers_selected.as_deref().is_some_and(|id| {
                                id != OPENCODE_PROVIDER
                                    && !providers.iter().any(|provider| provider.id == id)
                            });
                        let page_reload = this.providers_selected.is_some();
                        this.providers_store = providers;
                        if selected_missing {
                            this.providers_selected = None;
                            this.providers_adding = false;
                            this.providers_renaming = false;
                            this.providers_model_editor = None;
                            this.providers_model_editor_modalities.clear();
                            this.providers_delete_arming = None;
                        } else if page_reload && let Some(id) = this.providers_selected.clone() {
                            this.load_provider_fields(&id, cx);
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        this.show_toast(tr!("providers.sync_failed", error = error.to_string()));
                    }
                }
            });
        })
        .detach();
    }

    /// Commit the working roster straight into OpenCode's configuration, then
    /// re-probe so the composer's picker picks up the new catalog. OpenCode
    /// watches the file and hot-reloads, so running serves pick the change up
    /// without a restart.
    pub(super) fn commit_custom_providers(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = fintwind_client::opencode_config::save_providers(&self.providers_store) {
            self.show_toast(tr!("providers.sync_failed", error = error.to_string()));
        }
        self.refresh_provider_detection();
        cx.notify();
    }

    /// Debounced commit for keystroke-level edits (base URL, API key), so a
    /// burst of typing costs one save and one probe instead of one per key.
    fn schedule_custom_providers_commit(&mut self, cx: &mut Context<Self>) {
        self.providers_commit_generation += 1;
        let generation = self.providers_commit_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(700))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.providers_commit_generation != generation {
                    return;
                }
                this.providers_commit_generation = 0;
                this.commit_custom_providers(cx);
            });
        })
        .detach();
    }

    /// A detail field was edited. Applies the new value to the selected
    /// provider; the `Edited` echo from loading fields is filtered here.
    pub(super) fn provider_field_edited(
        &mut self,
        field: ProviderField,
        value: String,
        cx: &mut Context<Self>,
    ) {
        let selected = self.providers_selected.clone();
        let Some(provider) = self
            .providers_store
            .iter_mut()
            .find(|provider| Some(&provider.id) == selected.as_ref())
        else {
            return;
        };
        let changed = match field {
            ProviderField::BaseUrl => {
                if provider.base_url == value {
                    return;
                }
                provider.base_url = value;
                true
            }
            ProviderField::ApiKey => {
                if provider.api_key == value {
                    return;
                }
                provider.api_key = value;
                true
            }
        };
        if changed {
            // The endpoint or key just changed: this provider's connectivity
            // and first-token verdicts no longer describe it.
            let provider_id = provider.id.clone();
            self.clear_provider_probe_results(&provider_id);
            self.schedule_custom_providers_commit(cx);
            cx.notify();
        }
    }

    // ── Mutations ──────────────────────────────────────────────────────────

    fn begin_add_provider(&mut self, cx: &mut Context<Self>) {
        self.providers_adding = true;
        self.providers_renaming = false;
        self.providers_delete_arming = None;
        self.providers_model_editor = None;
        self.providers_model_editor_modalities.clear();
        self.providers_api_key_revealed = false;
        self.providers_form_format = ProviderApiFormat::default();
        self.providers_form_models.clear();
        // A fresh blank form: an in-flight fetch or probe from a previous
        // visit must not land its result here.
        self.providers_form_model_catalog.clear();
        self.providers_form_connectivity = None;
        self.providers_form_fetch_generation += 1;
        self.providers_form_probe_generation += 1;
        for input in [
            &self.provider_form_name,
            &self.provider_form_base_url,
            &self.provider_form_api_key,
        ] {
            input.update(cx, |input, cx| input.set_content(String::new(), cx));
        }
        cx.notify();
    }

    pub(super) fn submit_provider_form(&mut self, cx: &mut Context<Self>) {
        if !self.providers_adding {
            return;
        }
        let name = self.provider_form_name.read(cx).content().trim().to_owned();
        let base_url = self
            .provider_form_base_url
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let api_key = self
            .provider_form_api_key
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let mut models = Vec::new();
        for draft in &self.providers_form_models {
            let id = draft.id.read(cx).content().trim().to_owned();
            if id.is_empty() {
                continue;
            }
            let context = draft.context.read(cx).content();
            // A fetch's merge knows more about this id than the fields can
            // hold — its display name and output limit ride along.
            let known = self.providers_form_model_catalog.get(&id);
            models.push(CustomProviderModel {
                id,
                context_window: custom_providers::parse_context_window(&context),
                name: known.and_then(|model| model.name.clone()),
                output_limit: known.and_then(|model| model.output_limit),
                input_modalities: draft.input_modalities.clone(),
            });
        }

        if name.is_empty() {
            return;
        }
        if !custom_providers::base_url_valid(&base_url) {
            self.show_toast(tr!("providers.hint_invalid_url"));
            return;
        }
        if models.is_empty() {
            self.show_toast(tr!("providers.hint_need_model"));
            return;
        }

        let slugs: Vec<String> = self
            .providers_store
            .iter()
            .map(|provider| provider.slug.clone())
            .collect();
        let provider = CustomProvider::new(
            custom_providers::unique_provider_slug(&name, &slugs),
            name.clone(),
            base_url,
            self.providers_form_format,
            api_key,
            models,
        );
        let id = provider.id.clone();
        self.providers_store.push(provider);
        self.commit_custom_providers(cx);
        self.exit_provider_form(cx);
        self.select_provider(id, cx);
        self.show_success_toast(tr!("providers.added_toast", name = name));
    }

    fn delete_provider(&mut self, id: String, cx: &mut Context<Self>) {
        let name = self
            .providers_store
            .iter()
            .find(|provider| provider.id == id)
            .map(|provider| provider.name.clone());
        self.providers_store.retain(|provider| provider.id != id);
        self.providers_delete_arming = None;
        self.clear_provider_probe_results(&id);
        self.commit_custom_providers(cx);
        if self.providers_selected.as_deref() == Some(id.as_str()) {
            self.select_provider(OPENCODE_PROVIDER.to_owned(), cx);
        }
        if let Some(name) = name {
            self.show_success_toast(tr!("providers.deleted_toast", name = name));
        }
    }

    fn toggle_provider_enabled(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(provider) = self
            .providers_store
            .iter_mut()
            .find(|provider| provider.id == id)
        {
            provider.enabled = !provider.enabled;
        }
        self.commit_custom_providers(cx);
    }

    fn set_provider_format(
        &mut self,
        id: String,
        format: ProviderApiFormat,
        cx: &mut Context<Self>,
    ) {
        if let Some(provider) = self
            .providers_store
            .iter_mut()
            .find(|provider| provider.id == id)
        {
            if provider.api_format == format {
                cx.notify();
                return;
            }
            provider.api_format = format;
            // An explicit format choice owns the `npm` package key on the
            // next commit; without this the entry's original package would
            // be preserved.
            provider.npm_touched = true;
            self.commit_custom_providers(cx);
        }
    }

    fn begin_rename(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.selected_custom_provider() else {
            return;
        };
        self.providers_renaming = true;
        self.provider_rename_input
            .update(cx, |input, cx| input.set_content(provider.name, cx));
        cx.notify();
    }

    pub(super) fn confirm_rename(&mut self, cx: &mut Context<Self>) {
        if !self.providers_renaming {
            return;
        }
        let name = self
            .provider_rename_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        self.providers_renaming = false;
        let selected = self.providers_selected.clone();
        if let Some(provider) = self
            .providers_store
            .iter_mut()
            .find(|provider| Some(&provider.id) == selected.as_ref())
        {
            if !name.is_empty() && provider.name != name {
                provider.name = name;
                self.commit_custom_providers(cx);
                return;
            }
        }
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.providers_renaming = false;
        cx.notify();
    }

    fn begin_model_editor(&mut self, editor: ProvidersModelEditor, cx: &mut Context<Self>) {
        let (id, context, modalities) = match (&editor, self.selected_custom_provider()) {
            (ProvidersModelEditor::Edit(index), Some(provider)) => {
                let Some(model) = provider.models.get(*index) else {
                    return;
                };
                (
                    model.id.clone(),
                    model
                        .context_window
                        .map(|window| custom_providers::format_context_window(window))
                        .unwrap_or_default(),
                    self.effective_input_modalities(model),
                )
            }
            _ => (String::new(), String::new(), Vec::new()),
        };
        self.providers_model_editor = Some(editor);
        self.providers_model_editor_modalities = modalities;
        self.provider_model_id_input
            .update(cx, |input, cx| input.set_content(id, cx));
        self.provider_model_context_input
            .update(cx, |input, cx| input.set_content(context, cx));
        cx.notify();
    }

    /// The input modalities a model row shows: the recorded list when there is
    /// one, otherwise the metadata table's answer for the id. The editor seeds
    /// its picker from this, so it opens showing what the row currently says.
    fn effective_input_modalities(&self, model: &CustomProviderModel) -> Vec<String> {
        if !model.input_modalities.is_empty() {
            return model.input_modalities.clone();
        }
        self.models_dev_table
            .as_deref()
            .map(|table| table.resolve_input_modalities(&model.id))
            .unwrap_or_default()
    }

    fn cancel_model_editor(&mut self, cx: &mut Context<Self>) {
        self.providers_model_editor = None;
        self.providers_model_editor_modalities.clear();
        cx.notify();
    }

    pub(super) fn confirm_model_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.providers_model_editor.clone() else {
            return;
        };
        let model_id = self
            .provider_model_id_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        if model_id.is_empty() {
            return;
        }
        let Some(provider) = self.selected_custom_provider() else {
            return;
        };
        let duplicates = provider.models.iter().any(|model| {
            model.id == model_id
                && match &editor {
                    ProvidersModelEditor::Add => true,
                    ProvidersModelEditor::Edit(index) => {
                        provider.models.get(*index).map(|model| model.id.as_str())
                            != Some(model_id.as_str())
                    }
                }
        });
        if duplicates {
            self.show_toast(tr!("providers.duplicate_model"));
            return;
        }
        let context_text = self.provider_model_context_input.read(cx).content();
        let context_window = match custom_providers::parse_context_window(&context_text) {
            Some(window) => Some(window),
            None if context_text.trim().is_empty() => None,
            None => {
                self.show_toast(tr!("providers.hint_invalid_window"));
                return;
            }
        };
        let mut model = CustomProviderModel {
            id: model_id.clone(),
            context_window,
            input_modalities: self.providers_model_editor_modalities.clone(),
            ..Default::default()
        };
        if let Some(provider) = self
            .providers_store
            .iter_mut()
            .find(|candidate| candidate.id == provider.id)
        {
            match editor {
                ProvidersModelEditor::Add => provider.models.push(model),
                ProvidersModelEditor::Edit(index) => {
                    if let Some(slot) = provider.models.get_mut(index) {
                        // The editor only owns the id, the context window, and
                        // the modalities; catalog-filled basics ride along
                        // unless the id now names a different model.
                        if slot.id == model_id {
                            model.name = slot.name.take();
                            model.output_limit = slot.output_limit;
                        }
                        *slot = model;
                    }
                }
            }
        }
        self.providers_model_editor = None;
        self.providers_model_editor_modalities.clear();
        self.commit_custom_providers(cx);
    }

    fn delete_model(&mut self, id: String, index: usize, cx: &mut Context<Self>) {
        let mut removed = false;
        if let Some(provider) = self
            .providers_store
            .iter_mut()
            .find(|provider| provider.id == id)
        {
            // A provider entry with no models is rejected by OpenCode's
            // config validation, so the roster never holds one; removing the
            // provider itself is the explicit escape hatch.
            if index < provider.models.len() {
                if provider.models.len() == 1 {
                    self.show_toast(tr!("providers.hint_need_model"));
                    cx.notify();
                    return;
                }
                provider.models.remove(index);
                removed = true;
            }
        }
        match self.providers_model_editor {
            Some(ProvidersModelEditor::Edit(edited)) if edited == index => {
                self.providers_model_editor = None;
                self.providers_model_editor_modalities.clear();
            }
            // The editor points at a slot in a shrinking list: follow the
            // model it opened on rather than whatever shifts into the index.
            Some(ProvidersModelEditor::Edit(edited)) if removed && edited > index => {
                self.providers_model_editor = Some(ProvidersModelEditor::Edit(edited - 1));
            }
            _ => {}
        }
        self.commit_custom_providers(cx);
    }

    fn add_form_model_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.model_id_placeholder"))
        });
        let context = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .search_field()
                .placeholder(tr!("providers.context_window_placeholder"))
        });
        self.providers_form_models.push(ProviderFormModelDraft {
            id,
            context,
            input_modalities: Vec::new(),
        });
        cx.notify();
    }

    pub(super) fn selected_custom_provider(&self) -> Option<CustomProvider> {
        let selected = self.providers_selected.clone();
        self.providers_store
            .iter()
            .find(|provider| Some(&provider.id) == selected.as_ref())
            .cloned()
    }

    // ── Page ───────────────────────────────────────────────────────────────

    pub(super) fn render_providers_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        div()
            .size_full()
            .min_h_0()
            .flex()
            .child(self.render_providers_list_pane(&theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.render_providers_detail(&theme, cx)),
            )
            .into_any_element()
    }

    fn render_providers_list_pane(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let accent = providers_accent(theme);
        let selected_id = self.effective_provider_id();
        let mut rows = div().px(px(8.0)).flex().flex_col();

        rows = rows.child(section_label(theme, tr!("providers.section_builtin"), true));
        rows = rows.child(self.render_provider_list_row(
            "provider-row-builtin",
            OPENCODE_PROVIDER.to_owned(),
            "OpenCode".to_owned(),
            "icons/provider-opencode.svg",
            selected_id == OPENCODE_PROVIDER,
            self.provider_probe().is_some_and(|probe| probe.installed),
            theme,
            accent,
            cx,
        ));

        rows = rows.child(section_label(theme, tr!("providers.section_custom"), false));
        for (index, provider) in self.providers_store.iter().enumerate() {
            rows = rows.child(self.render_provider_list_row(
                SharedString::from(format!("provider-row-{index}")),
                provider.id.clone(),
                provider.name.clone(),
                provider_icon(&provider.name),
                selected_id == provider.id,
                provider.enabled,
                theme,
                accent,
                cx,
            ));
        }
        rows = rows.child(self.render_add_provider_row(theme, cx));

        div()
            .w(px(PROVIDERS_LIST_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .pt(px(14.0))
                    .px(px(16.0))
                    .flex_none()
                    .text_size(ui_px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("settings.providers")),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mt(px(6.0))
                    .relative()
                    .child(
                        div()
                            .id("providers-list-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.providers_list_scroll)
                            .pb(px(8.0))
                            .child(rows),
                    )
                    .child(scrollbar::vertical(
                        &self.providers_list_scroll,
                        &self.providers_list_scrollbar,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(26.0))
                    .px(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .text_size(ui_px(9.5))
                    .text_color(theme.text_ghost)
                    .child(self.providers_footer_caption()),
            )
    }

    fn providers_footer_caption(&self) -> SharedString {
        let count = self.providers_store.len();
        SharedString::from(match count {
            0 => tr!("providers.custom_count_zero"),
            1 => tr!("providers.custom_count_one", count = count),
            _ => tr!("providers.custom_count_many", count = count),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_provider_list_row(
        &self,
        id: impl Into<ElementId>,
        provider_id: String,
        label: String,
        icon_path: &'static str,
        selected: bool,
        active: bool,
        theme: &Theme,
        accent: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .child(
                div()
                    .id(id)
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(accent))
                    .w_full()
                    .px(px(9.0))
                    .py(px(7.0))
                    .mb(px(1.0))
                    .rounded(px(8.0))
                    .cursor_default()
                    .when(selected, |element| element.bg(accent.opacity(0.13)))
                    .when(!selected, |element| {
                        element
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                    })
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(
                        div()
                            .w(px(28.0))
                            .h(px(28.0))
                            .flex_none()
                            // Concentric with the row: 8px row radius minus
                            // the 7-9px row padding leaves almost no arc.
                            .rounded(px(2.0))
                            .bg(theme.overlay)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(
                                icon_path,
                                14.0,
                                if selected {
                                    accent
                                } else {
                                    theme.text_secondary
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if selected || active {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(label),
                    )
                    .child(
                        div()
                            .w(px(8.0))
                            .h(px(8.0))
                            .flex_none()
                            .rounded_full()
                            .bg(if active { accent } else { theme.text_ghost }),
                    )
                    .on_click({
                        let provider_id = provider_id.clone();
                        cx.listener(move |this, _, _, cx| {
                            this.select_provider(provider_id.clone(), cx);
                        })
                    })
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.select_provider(provider_id.clone(), cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .into_any_element()
    }

    fn render_add_provider_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let accent = providers_accent(theme);
        let adding = self.providers_adding;
        div()
            .child(
                div()
                    .id("add-provider-row")
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(accent))
                    .w_full()
                    .px(px(9.0))
                    .py(px(7.0))
                    .mt(px(10.0))
                    .rounded(px(8.0))
                    .cursor_default()
                    .border_1()
                    .border_color(if adding { accent } else { theme.border_strong })
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .text_color(if adding {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(!adding, |element| {
                        element
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                            .active(|element| element.bg(theme.overlay_strong))
                    })
                    .child(icon(
                        "icons/plus.svg",
                        13.0,
                        if adding { accent } else { theme.text_tertiary },
                    ))
                    .child(
                        div()
                            .text_size(ui_px(12.5))
                            .child(tr!("providers.add_provider")),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.begin_add_provider(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.begin_add_provider(cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .into_any_element()
    }

    // ── Detail pane ────────────────────────────────────────────────────────

    fn render_providers_detail(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.providers_adding {
            return self.render_provider_form(theme, cx);
        }
        if self.effective_provider_id() != OPENCODE_PROVIDER {
            if let Some(provider) = self.selected_custom_provider() {
                return self.render_custom_provider_detail(&provider, theme, cx);
            }
        }
        self.render_builtin_provider_detail(theme, cx)
    }

    fn scrollable_detail(&self, content: Div) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                div()
                    .id("providers-detail-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.providers_detail_scroll)
                    .px(px(24.0))
                    .pt(px(18.0))
                    .pb(px(24.0))
                    .child(content),
            )
            .child(scrollbar::vertical(
                &self.providers_detail_scroll,
                &self.providers_detail_scrollbar,
            ))
            .into_any_element()
    }

    /// The built-in OpenCode provider: detection status, a live refresh, and
    /// its catalog. Its endpoint and key belong to the CLI, so the pane says
    /// so instead of offering fake fields.
    fn render_builtin_provider_detail(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let checking = self.provider_detection_remaining > 0;
        let probe = self.provider_probe();
        let installed = probe.is_some_and(|probe| probe.installed);
        let version = self
            .provider_versions
            .get(OPENCODE_PROVIDER)
            .and_then(|version| version.clone());
        let model_count = probe.map(|probe| probe.models.len()).unwrap_or(0);

        let refresh = outline_button(
            "refresh-builtin-providers",
            if checking {
                tr!("common.checking")
            } else {
                tr!("common.refresh")
            },
            Some("icons/rotate-cw.svg"),
            theme,
        )
        .opacity(if checking { 0.6 } else { 1.0 })
        .when(!checking, |element| {
            element
                .on_click(cx.listener(|this, _, _, cx| {
                    this.refresh_provider_detection();
                    cx.notify();
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.refresh_provider_detection();
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
        });

        let mut header_title = div().flex().items_baseline().gap(px(7.0)).child(
            div()
                .text_size(ui_px(15.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child("OpenCode"),
        );
        if let Some(version) = version {
            header_title = header_title.child(
                div()
                    .font_family(crate::md::render::MONO_FAMILY)
                    .text_size(ui_px(10.0))
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(format!("v{version}"))),
            );
        }

        let mut model_rows = div().flex().flex_col();
        if model_count == 0 {
            model_rows = model_rows.child(
                div()
                    .px(px(10.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon("icons/info.svg", 12.0, theme.text_tertiary))
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .line_height(ui_px(15.0))
                            .text_color(theme.text_tertiary)
                            .child(tr!("providers.not_detected_as", command = "opencode")),
                    ),
            );
        }
        if let Some(probe) = probe {
            for (index, model) in probe.models.iter().enumerate() {
                // The CLI's catalog qualifies ids with their sub-provider
                // (`anthropic/claude-…`); the metadata table knows bare ids.
                let bare_id = model.id.rsplit('/').next().unwrap_or(&model.id);
                let modality_pill = self.model_modality_pill(
                    theme,
                    bare_id,
                    &[],
                    SharedString::from(format!("builtin-modality-{index}")),
                );
                model_rows = model_rows.child(
                    div()
                        .px(px(10.0))
                        .h(px(36.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .when(index > 0, |element| {
                            element.border_t_1().border_color(theme.border)
                        })
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui_px(11.0))
                                .text_color(theme.text)
                                .child(SharedString::from(model.name.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(crate::md::render::MONO_FAMILY)
                                .text_size(ui_px(9.5))
                                .text_color(theme.text_tertiary)
                                .child(SharedString::from(model.id.clone())),
                        )
                        .children(modality_pill)
                        .when(model.is_default, |element| {
                            element.child(small_pill(
                                theme,
                                tr!("providers.default_model_badge"),
                                None,
                            ))
                        }),
                );
            }
        }

        self.scrollable_detail(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(provider_tile(
                            theme,
                            "icons/provider-opencode.svg",
                            installed,
                        ))
                        .child(
                            div().flex_1().min_w_0().child(header_title).child(
                                div()
                                    .mt(px(2.0))
                                    .text_size(ui_px(10.5))
                                    .text_color(theme.text_tertiary)
                                    .child(if installed {
                                        SharedString::from(if model_count == 1 {
                                            tr!("providers.model_count_one", count = model_count)
                                        } else {
                                            tr!("providers.model_count_many", count = model_count)
                                        })
                                    } else {
                                        SharedString::from(tr!(
                                            "providers.not_detected_as",
                                            command = "opencode"
                                        ))
                                    }),
                            ),
                        )
                        .child(refresh),
                )
                .child(
                    div()
                        .mt(px(14.0))
                        .text_size(ui_px(11.5))
                        .line_height(ui_px(17.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("providers.description")),
                )
                .child(info_note(
                    theme,
                    "icons/info.svg",
                    tr!("providers.builtin_managed"),
                ))
                .child(div().mt(px(18.0)).child(section_label(
                    theme,
                    tr!("providers.models_label"),
                    true,
                )))
                .child(
                    div()
                        .mt(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .rounded(px(9.0))
                        .overflow_hidden()
                        .child(model_rows),
                ),
        )
    }

    fn render_custom_provider_detail(
        &self,
        provider: &CustomProvider,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = providers_accent(theme);
        let enabled = provider.enabled;
        let id = provider.id.clone();

        // Header: tile, name (or rename editor), enable badge, and actions.
        let name_area: AnyElement = if self.providers_renaming {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    TextField::new("provider-rename-field", self.provider_rename_input.clone())
                        .w(px(220.0)),
                )
                .child(
                    small_action_button(
                        "confirm-provider-rename",
                        "icons/check.svg",
                        tr!("providers.save"),
                        accent,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_rename(cx);
                    }))
                    .on_key_down(cx.listener(
                        |this, event: &KeyDownEvent, _, cx| {
                            if !event.keystroke.modifiers.modified()
                                && matches!(event.keystroke.key.as_str(), "enter" | "space")
                            {
                                this.confirm_rename(cx);
                                cx.stop_propagation();
                            }
                        },
                    )),
                )
                .child(
                    small_action_button(
                        "cancel-provider-rename",
                        "icons/x.svg",
                        tr!("common.cancel"),
                        theme.text_secondary,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_rename(cx);
                    })),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(ui_px(15.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(provider.name.clone())),
                )
                .child(
                    icon_button("rename-provider", "icons/pencil.svg", *theme)
                        .tooltip(Tooltip::text(tr!("providers.rename")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.begin_rename(cx);
                        })),
                )
                .child(enabled_badge(theme, accent, enabled))
                .into_any_element()
        };

        let toggle_button = outline_button(
            "toggle-provider-enabled",
            if enabled {
                tr!("providers.action_disable")
            } else {
                tr!("providers.action_enable")
            },
            None,
            theme,
        )
        .on_click({
            let id = id.clone();
            cx.listener(move |this, _, _, cx| {
                this.toggle_provider_enabled(id.clone(), cx);
            })
        })
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
            if !event.keystroke.modifiers.modified()
                && matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                this.toggle_provider_enabled(id.clone(), cx);
                cx.stop_propagation();
            }
        }));

        let armed = self.providers_delete_arming.as_deref() == Some(provider.id.as_str());
        let delete_button = div()
            .id("delete-provider")
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .h(px(30.0))
            .px(px(10.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(if armed {
                theme.danger
            } else {
                theme.border_strong
            })
            .when(armed, |element| element.bg(theme.danger.opacity(0.12)))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(ui_px(12.0))
            .text_color(if armed {
                theme.danger
            } else {
                theme.text_secondary
            })
            .hover(|element| element.bg(theme.overlay).text_color(theme.danger))
            .active(|element| element.bg(theme.danger.opacity(0.18)).text_color(theme.danger))
            .child(icon(
                "icons/trash.svg",
                12.5,
                if armed {
                    theme.danger
                } else {
                    theme.text_tertiary
                },
            ))
            .child(if armed {
                tr!("providers.confirm_delete_provider")
            } else {
                tr!("providers.delete_provider")
            })
            .on_click(cx.listener({
                let id = provider.id.clone();
                move |this, _, _, cx| {
                    if this.providers_delete_arming.as_deref() == Some(id.as_str()) {
                        this.delete_provider(id.clone(), cx);
                    } else {
                        this.providers_delete_arming = Some(id.clone());
                        cx.notify();
                    }
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.providers_delete_arming.take().is_some() {
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener({
                let id = provider.id.clone();
                move |this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        if this.providers_delete_arming.as_deref() == Some(id.as_str()) {
                            this.delete_provider(id.clone(), cx);
                        } else {
                            this.providers_delete_arming = Some(id.clone());
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }
                }
            }));

        // Endpoint fields.
        let base_url_field = labeled_field(
            theme,
            tr!("providers.base_url_label"),
            TextField::new(
                "provider-base-url-field",
                self.provider_base_url_input.clone(),
            )
            .w_full(),
        );
        let api_key_field = labeled_field(
            theme,
            tr!("providers.api_key_label"),
            self.render_api_key_field(theme, cx),
        );

        // API format dropdown.
        let current_format = provider.api_format;
        let weak = cx.entity().downgrade();
        let format_handle = self.menu_handle("provider-format-selector", cx);
        let format_selector = dropdown_menu(
            MenuChip::new("provider-format-selector")
                .label(api_format_label(current_format))
                .outlined()
                .selected(format_handle.is_open())
                .w(px(280.0))
                .justify_between(),
            "provider-format-selector-menu",
            &format_handle,
            MenuAlign::BelowLeft,
            move |_| {
                ProviderApiFormat::ALL
                    .into_iter()
                    .map(|format| {
                        let weak = weak.clone();
                        MenuItem::new(api_format_label(format), move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                if let Some(id) = this.providers_selected.clone() {
                                    this.set_provider_format(id, format, cx);
                                }
                            });
                        })
                        .selected(format == current_format)
                    })
                    .collect()
            },
        );
        let format_field = labeled_field(theme, tr!("providers.api_format_label"), format_selector);
        let connectivity_field = self.render_connectivity_field(provider, theme, cx);

        let models_section = self.render_provider_models(provider, theme, cx);

        self.scrollable_detail(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(provider_tile(theme, provider_icon(&provider.name), enabled))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .child(name_area),
                        )
                        .child(toggle_button)
                        .child(delete_button),
                )
                .child(
                    div()
                        .mt(px(6.0))
                        .pl(px(50.0))
                        .text_size(ui_px(10.5))
                        .truncate()
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(provider.slug.clone())),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .child(base_url_field)
                        .child(api_key_field)
                        .child(format_field)
                        .child(connectivity_field),
                )
                .child(models_section),
        )
    }

    /// The API key row: masked dots while hidden (the daemon token's
    /// treatment), the editable field itself while revealed.
    fn render_api_key_field(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let revealed = self.providers_api_key_revealed;
        let field: AnyElement = if revealed {
            TextField::new(
                "provider-api-key-field",
                self.provider_api_key_input.clone(),
            )
            .w_full()
            .into_any_element()
        } else {
            let key = self.provider_api_key_input.read(cx).content();
            let masked = if key.is_empty() {
                tr!("providers.api_key_placeholder")
            } else {
                "•".repeat(key.chars().count().min(36))
            };
            div()
                .h(px(28.0))
                .w_full()
                .px(px(8.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.inset)
                .flex()
                .items_center()
                .gap(px(6.0))
                .cursor_default()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(crate::md::render::MONO_FAMILY)
                        .text_size(ui_px(11.0))
                        .text_color(if key.is_empty() {
                            theme.text_ghost
                        } else {
                            theme.text_secondary
                        })
                        .child(SharedString::from(masked)),
                )
                .child(
                    icon_button("reveal-provider-key-edit", "icons/pencil.svg", *theme)
                        .tooltip(Tooltip::text(tr!("common.rename")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.providers_api_key_revealed = true;
                            cx.notify();
                        })),
                )
                .into_any_element()
        };

        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(div().flex_1().min_w_0().child(field))
            .child(
                icon_button(
                    "toggle-provider-key-reveal",
                    if revealed {
                        "icons/eye-off.svg"
                    } else {
                        "icons/eye.svg"
                    },
                    *theme,
                )
                .tooltip(Tooltip::text(if revealed {
                    tr!("daemon.hide_token")
                } else {
                    tr!("daemon.reveal_token")
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.providers_api_key_revealed = !this.providers_api_key_revealed;
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    fn render_provider_models(
        &self,
        provider: &CustomProvider,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows = div().flex().flex_col();
        if provider.models.is_empty() {
            rows = rows.child(
                div()
                    .px(px(10.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon("icons/info.svg", 12.0, theme.text_tertiary))
                    .child(
                        div()
                            .text_size(ui_px(10.5))
                            .line_height(ui_px(15.0))
                            .text_color(theme.text_tertiary)
                            .child(tr!("providers.models_empty")),
                    ),
            );
        }
        // Each visible element after the first carries a top hairline, so an
        // editor row sliding in between models never doubles a border.
        let mut separator = !provider.models.is_empty();
        for (index, model) in provider.models.iter().enumerate() {
            if self.providers_model_editor.as_ref() == Some(&ProvidersModelEditor::Edit(index)) {
                rows = rows.child(self.render_model_editor_row(theme, cx, separator));
                separator = true;
                continue;
            }
            let id = provider.id.clone();
            // A models.dev-filled name reads as the model's title; the raw id
            // stays beside it in mono. Without a name the id is the title.
            let named = model.display_name();
            let modality_pill = self.model_modality_pill(
                theme,
                &model.id,
                &model.input_modalities,
                SharedString::from(format!("modality-{index}")),
            );
            let latency_cluster = self.render_model_latency_cluster(
                &provider.id,
                &model.id,
                index,
                theme,
                cx,
            );
            rows = rows.child(
                div()
                    .px(px(10.0))
                    .h(px(38.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .when(separator, |element| {
                        element.border_t_1().border_color(theme.border)
                    })
                    .when_some(named, |element, name| {
                        element.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui_px(11.0))
                                .text_color(theme.text)
                                .child(SharedString::from(name.to_owned())),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::md::render::MONO_FAMILY)
                            .text_size(ui_px(if named.is_some() { 9.5 } else { 11.0 }))
                            .text_color(if named.is_some() {
                                theme.text_tertiary
                            } else {
                                theme.text
                            })
                            .child(SharedString::from(model.id.clone())),
                    )
                    .children(modality_pill)
                    .when_some(model.context_window, |element, window| {
                        element.child(small_pill(
                            theme,
                            custom_providers::format_context_window(window),
                            None,
                        ))
                    })
                    .child(div().flex_1())
                    .child(latency_cluster)
                    .child(
                        icon_button(
                            SharedString::from(format!("edit-model-{index}")),
                            "icons/pencil.svg",
                            *theme,
                        )
                        .tooltip(Tooltip::text(tr!("providers.edit_model")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.begin_model_editor(ProvidersModelEditor::Edit(index), cx);
                        })),
                    )
                    .child(
                        icon_button(
                            SharedString::from(format!("delete-model-{index}")),
                            "icons/trash.svg",
                            *theme,
                        )
                        .tooltip(Tooltip::text(tr!("providers.delete_model")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.delete_model(id.clone(), index, cx);
                        })),
                    ),
            );
            separator = true;
        }

        if self.providers_model_editor == Some(ProvidersModelEditor::Add) {
            rows = rows.child(self.render_model_editor_row(theme, cx, separator));
        } else {
            rows = rows.child(
                div()
                    .id("add-provider-model")
                    .tab_index(0)
                    .focus_visible(|style| style.border_color(providers_accent(theme)))
                    .h(px(36.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .px(px(10.0))
                    .when(separator, |element| {
                        element.border_t_1().border_color(theme.border)
                    })
                    .cursor_default()
                    .text_size(ui_px(12.0))
                    .text_color(theme.text_secondary)
                    .hover(|element| element.bg(theme.overlay))
                    .child(icon("icons/plus.svg", 12.5, theme.text_tertiary))
                    .child(tr!("providers.add_model"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.begin_model_editor(ProvidersModelEditor::Add, cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.begin_model_editor(ProvidersModelEditor::Add, cx);
                            cx.stop_propagation();
                        }
                    })),
            );
        }

        div()
            .mt(px(18.0))
            .child(self.render_models_section_header(theme, cx))
            .child(
                div()
                    .mt(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(9.0))
                    .overflow_hidden()
                    .child(rows),
            )
            .into_any_element()
    }

    /// The inline model editor: id, context window, and input modalities,
    /// confirmed with the check button or Enter in either field.
    fn render_model_editor_row(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
        separator: bool,
    ) -> AnyElement {
        let accent = providers_accent(theme);
        let model_id = self
            .provider_model_id_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let context_text = self
            .provider_model_context_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let valid = !model_id.is_empty()
            && (context_text.is_empty()
                || custom_providers::parse_context_window(&context_text).is_some());
        let toggle = |this: &mut Self, modality: &str, cx: &mut Context<Self>| {
            toggle_modality(&mut this.providers_model_editor_modalities, modality);
            cx.notify();
        };

        div()
            .px(px(10.0))
            .py(px(8.0))
            .bg(theme.inset)
            .flex()
            .flex_col()
            .gap(px(8.0))
            .when(separator, |element| {
                element.border_t_1().border_color(theme.border)
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        TextField::new(
                            "provider-model-id-field",
                            self.provider_model_id_input.clone(),
                        )
                        .flex_1()
                        .min_w_0(),
                    )
                    .child(
                        TextField::new(
                            "provider-model-context-field",
                            self.provider_model_context_input.clone(),
                        )
                        .w(px(120.0)),
                    )
                    .child(
                        div()
                            .id("confirm-provider-model")
                            .tab_index(0)
                            .focus_visible(|style| style.border_color(accent))
                            .h(px(30.0))
                            .px(px(10.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .cursor_default()
                            .text_size(ui_px(12.0))
                            .when(valid, |element| {
                                element
                                    .bg(accent)
                                    .text_color(on_providers_accent(theme))
                                    .hover(|element| element.bg(accent.opacity(0.85)))
                                    .active(|element| element.bg(accent.opacity(0.72)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_model_editor(cx);
                                    }))
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, _, cx| {
                                            if !event.keystroke.modifiers.modified()
                                                && matches!(
                                                    event.keystroke.key.as_str(),
                                                    "enter" | "space"
                                                )
                                            {
                                                this.confirm_model_editor(cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    ))
                            })
                            .when(!valid, |element| {
                                element
                                    .border_1()
                                    .border_color(theme.border_strong)
                                    .text_color(theme.text_ghost)
                            })
                            .child(icon(
                                "icons/check.svg",
                                12.5,
                                if valid {
                                    on_providers_accent(theme)
                                } else {
                                    theme.text_ghost
                                },
                            ))
                            .child(tr!("providers.save")),
                    )
                    .child(
                        small_action_button(
                            "cancel-provider-model",
                            "icons/x.svg",
                            tr!("common.cancel"),
                            theme.text_secondary,
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_model_editor(cx);
                        })),
                    ),
            )
            .child(self.render_modality_row(
                theme,
                cx,
                "model-editor",
                &self.providers_model_editor_modalities,
                true,
                toggle,
            ))
            .into_any_element()
    }

    /// The input-modality picker of a model editor: one toggle chip per
    /// modality, plus a hint that an empty selection defers to models.dev.
    /// The caller supplies `toggle`, so the editor and each add-form draft
    /// share the same control; `hint` is off on repeated draft rows, where
    /// the same sentence would read as noise.
    fn render_modality_row(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
        id_prefix: &str,
        selected: &[String],
        hint: bool,
        toggle: impl Fn(&mut Self, &str, &mut Context<Self>) + Copy + 'static,
    ) -> Div {
        let accent = providers_accent(theme);
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex_none()
                    .pr(px(2.0))
                    .text_size(ui_px(10.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_tertiary)
                    .child(tr!("providers.input_modalities_label")),
            );
        for modality in custom_providers::INPUT_MODALITIES {
            let active = selected.iter().any(|entry| entry == modality);
            row = row.child(
                modality_chip(
                    SharedString::from(format!("{id_prefix}-{modality}")),
                    theme,
                    accent,
                    modality,
                    active,
                )
                .on_click(cx.listener(move |this, _, _, cx| toggle(this, modality, cx)))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        toggle(this, modality, cx);
                        cx.stop_propagation();
                    }
                })),
            );
        }
        if hint {
            row = row.child(
                div()
                    .ml(px(2.0))
                    .text_size(ui_px(10.0))
                    .text_color(theme.text_ghost)
                    .child(tr!("providers.input_modalities_hint")),
            );
        }
        row
    }

    // ── Add-provider form ──────────────────────────────────────────────────

    fn render_provider_form(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let accent = providers_accent(theme);
        let name = self.provider_form_name.read(cx).content().trim().to_owned();
        let base_url = self
            .provider_form_base_url
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let mut model_count = 0usize;
        let mut drafts = div().flex().flex_col();
        let draft_count = self.providers_form_models.len();
        for (index, draft) in self.providers_form_models.iter().enumerate() {
            if !draft.id.read(cx).content().trim().is_empty() {
                model_count += 1;
            }
            let context_entity = draft.context.clone();
            let toggle = move |this: &mut Self, modality: &str, cx: &mut Context<Self>| {
                if let Some(draft) = this.providers_form_models.get_mut(index) {
                    toggle_modality(&mut draft.input_modalities, modality);
                }
                cx.notify();
            };
            drafts = drafts.child(
                div()
                    .py(px(8.0))
                    .pr(px(2.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .when(index > 0, |element| {
                        element.border_t_1().border_color(theme.border)
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                TextField::new(
                                    SharedString::from(format!("form-model-id-{index}")),
                                    draft.id.clone(),
                                )
                                .flex_1()
                                .min_w_0(),
                            )
                            .child(
                                TextField::new(
                                    SharedString::from(format!("form-model-context-{index}")),
                                    draft.context.clone(),
                                )
                                .w(px(120.0)),
                            )
                            .child(
                                icon_button(
                                    SharedString::from(format!("remove-form-model-{index}")),
                                    "icons/trash.svg",
                                    *theme,
                                )
                                .tooltip(Tooltip::text(tr!("providers.delete_model")))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        if let Some(index) = this
                                            .providers_form_models
                                            .iter()
                                            .position(|draft| draft.context == context_entity)
                                        {
                                            this.providers_form_models.remove(index);
                                            // A dropped draft's field may hold focus; let
                                            // it go instead of parking it nowhere.
                                            window.blur();
                                            cx.notify();
                                        }
                                    },
                                )),
                            ),
                    )
                    .child(self.render_modality_row(
                        theme,
                        cx,
                        &format!("form-model-{index}"),
                        &draft.input_modalities,
                        index == 0,
                        toggle,
                    )),
            );
        }
        let models_empty = draft_count == 0;
        let valid = !name.is_empty() && custom_providers::base_url_valid(&base_url) && model_count > 0;

        let current_format = self.providers_form_format;
        let weak = cx.entity().downgrade();
        let format_handle = self.menu_handle("provider-form-format-selector", cx);
        let format_selector = dropdown_menu(
            MenuChip::new("provider-form-format-selector")
                .label(api_format_label(current_format))
                .outlined()
                .selected(format_handle.is_open())
                .w(px(280.0))
                .justify_between(),
            "provider-form-format-selector-menu",
            &format_handle,
            MenuAlign::BelowLeft,
            move |_| {
                ProviderApiFormat::ALL
                    .into_iter()
                    .map(|format| {
                        let weak = weak.clone();
                        MenuItem::new(api_format_label(format), move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.providers_form_format = format;
                                // The form's probe spoke the old format's
                                // dialect; its verdict no longer stands.
                                this.providers_form_connectivity = None;
                                cx.notify();
                            });
                        })
                        .selected(format == current_format)
                    })
                    .collect()
            },
        );

        let submit_button = div()
            .id("submit-provider-form")
            .tab_index(0)
            .focus_visible(|style| style.border_color(accent))
            .h(px(32.0))
            .px(px(16.0))
            .rounded(px(8.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .text_size(ui_px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .when(valid, |element| {
                element
                    .bg(accent)
                    .text_color(on_providers_accent(theme))
                    .hover(|element| element.bg(accent.opacity(0.85)))
                    .active(|element| element.bg(accent.opacity(0.72)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.submit_provider_form(cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.submit_provider_form(cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            .when(!valid, |element| {
                element
                    .border_1()
                    .border_color(theme.border_strong)
                    .text_color(theme.text_ghost)
            })
            .child(tr!("providers.add_provider"));

        let hint: Option<AnyElement> = if !base_url.is_empty() && !custom_providers::base_url_valid(&base_url) {
            Some(form_hint(theme, accent, tr!("providers.hint_invalid_url")))
        } else if custom_providers::base_url_valid(&base_url) && model_count == 0 && !name.is_empty() {
            Some(form_hint(theme, accent, tr!("providers.hint_need_model")))
        } else {
            None
        };

        self.scrollable_detail(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(ui_px(15.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(tr!("providers.form_title")),
                )
                .child(
                    div()
                        .mt(px(4.0))
                        .text_size(ui_px(11.5))
                        .line_height(ui_px(17.0))
                        .text_color(theme.text_secondary)
                        .child(tr!("providers.form_description")),
                )
                .child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .child(labeled_field(
                            theme,
                            tr!("providers.name_label"),
                            TextField::new(
                                "provider-form-name-field",
                                self.provider_form_name.clone(),
                            )
                            .w_full(),
                        ))
                        .child(labeled_field(
                            theme,
                            tr!("providers.base_url_label"),
                            TextField::new(
                                "provider-form-base-url-field",
                                self.provider_form_base_url.clone(),
                            )
                            .w_full(),
                        ))
                        .child(labeled_field(
                            theme,
                            tr!("providers.api_key_label"),
                            TextField::new(
                                "provider-form-api-key-field",
                                self.provider_form_api_key.clone(),
                            )
                            .w_full(),
                        ))
                        .child(labeled_field(
                            theme,
                            tr!("providers.api_format_label"),
                            format_selector,
                        ))
                        .child(self.render_form_connectivity_field(theme, cx))
                        .child(
                            div()
                                .child(self.render_form_models_section_header(theme, cx))
                                .child(
                                    div()
                                        .mt(px(8.0))
                                        .border_1()
                                        .border_color(theme.border)
                                        .rounded(px(9.0))
                                        .overflow_hidden()
                                        .child(if models_empty {
                                            div()
                                                .px(px(10.0))
                                                .py(px(12.0))
                                                .flex()
                                                .items_center()
                                                .gap(px(8.0))
                                                .child(icon(
                                                    "icons/info.svg",
                                                    12.0,
                                                    theme.text_tertiary,
                                                ))
                                                .child(
                                                    div()
                                                        .text_size(ui_px(10.5))
                                                        .line_height(ui_px(15.0))
                                                        .text_color(theme.text_tertiary)
                                                        .child(tr!("providers.models_empty")),
                                                )
                                                .into_any_element()
                                        } else {
                                            drafts.into_any_element()
                                        }),
                                )
                                .child(
                                    div()
                                        .id("add-form-model")
                                        .tab_index(0)
                                        .focus_visible(|style| style.border_color(accent))
                                        .mt(px(8.0))
                                        .h(px(32.0))
                                        .px(px(10.0))
                                        .rounded(px(7.0))
                                        .border_1()
                                        .border_color(theme.border_strong)
                                        .flex()
                                        .items_center()
                                        .gap(px(7.0))
                                        .cursor_default()
                                        .text_size(ui_px(12.0))
                                        .text_color(theme.text_secondary)
                                        .hover(|element| {
                                            element.bg(theme.overlay).text_color(theme.text)
                                        })
                                        .child(icon("icons/plus.svg", 12.5, theme.text_tertiary))
                                        .child(tr!("providers.add_model"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.add_form_model_draft(window, cx);
                                        }))
                                        .on_key_down(cx.listener(
                                            |this, event: &KeyDownEvent, window, cx| {
                                                if !event.keystroke.modifiers.modified()
                                                    && matches!(
                                                        event.keystroke.key.as_str(),
                                                        "enter" | "space"
                                                    )
                                                {
                                                    this.add_form_model_draft(window, cx);
                                                    cx.stop_propagation();
                                                }
                                            },
                                        )),
                                ),
                        ),
                )
                .child(
                    div()
                        .mt(px(18.0))
                        .pt(px(14.0))
                        .border_t_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .children(hint)
                        .child(div().flex_1())
                        .child(
                            outline_button(
                                "cancel-provider-form",
                                tr!("common.cancel"),
                                None,
                                theme,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.exit_provider_form(cx);
                            })),
                        )
                        .child(submit_button),
                ),
        )
    }
}

// ── Small shared pieces ────────────────────────────────────────────────────

pub(super) fn section_label(theme: &Theme, label: String, first: bool) -> Div {
    div()
        .w_full()
        .pt(px(if first { 2.0 } else { 16.0 }))
        .pb(px(4.0))
        .px(px(9.0))
        .flex()
        .items_baseline()
        .text_size(ui_px(9.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_tertiary)
        .child(SharedString::from(label.to_uppercase()))
}

pub(super) fn provider_tile(theme: &Theme, icon_path: &'static str, active: bool) -> Div {
    div()
        .w(px(38.0))
        .h(px(38.0))
        .flex_none()
        .rounded(px(9.0))
        .bg(theme.overlay)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(
            icon_path,
            17.0,
            theme
                .text_secondary
                .opacity(if active { 1.0 } else { 0.45 }),
        ))
}

pub(super) fn enabled_badge(theme: &Theme, accent: Hsla, enabled: bool) -> Div {
    div()
        .px(px(7.0))
        .py(px(2.0))
        .rounded_full()
        .text_size(ui_px(9.5))
        .when(enabled, |element| {
            element.text_color(accent).bg(accent.opacity(0.14))
        })
        .when(!enabled, |element| {
            element.text_color(theme.text_tertiary).bg(theme.overlay)
        })
        .child(if enabled {
            tr!("providers.enabled_badge")
        } else {
            tr!("providers.disabled_badge")
        })
}

pub(super) fn small_pill(theme: &Theme, label: String, color: Option<Hsla>) -> Div {
    div()
        .px(px(6.0))
        .py(px(1.5))
        .rounded_full()
        .text_size(ui_px(9.0))
        .font_family(crate::md::render::MONO_FAMILY)
        .text_color(color.unwrap_or(theme.text_tertiary))
        .bg(theme.overlay)
        .child(SharedString::from(label))
}

// ── Modality badges ────────────────────────────────────────────────────────

/// The stroke icon for a catalog modality; `None` for anything the catalog
/// may add later — unknown modalities still appear in the pill's tooltip.
fn modality_icon_path(modality: &str) -> Option<&'static str> {
    match modality {
        "text" => Some("icons/modality-text.svg"),
        "image" => Some("icons/modality-image.svg"),
        "audio" => Some("icons/modality-audio.svg"),
        "video" => Some("icons/modality-video.svg"),
        "pdf" => Some("icons/modality-pdf.svg"),
        _ => None,
    }
}

/// The modality's word for the badge's tooltip.
fn modality_label(modality: &str) -> String {
    match modality {
        "text" => tr!("modality.text"),
        "image" => tr!("modality.image"),
        "audio" => tr!("modality.audio"),
        "video" => tr!("modality.video"),
        "pdf" => tr!("modality.pdf"),
        other => other.to_owned(),
    }
}

fn modality_labels(modalities: &[String]) -> String {
    modalities
        .iter()
        .map(|modality| modality_label(modality))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Flip one modality in a selection, keeping the canonical order the config
/// schema documents so saved lists do not shuffle between edits.
fn toggle_modality(selected: &mut Vec<String>, modality: &str) {
    if let Some(index) = selected.iter().position(|entry| entry == modality) {
        selected.remove(index);
        return;
    }
    selected.push(modality.to_owned());
    selected.sort_by_key(|entry| {
        custom_providers::INPUT_MODALITIES
            .iter()
            .position(|candidate| candidate == entry)
            .unwrap_or(usize::MAX)
    });
}

/// One toggle chip in a modality picker: the modality's stroke icon and word,
/// accent-filled while selected. The caller attaches the toggle handlers, so
/// the same chip serves the model editor and the add form's drafts.
fn modality_chip(
    id: impl Into<ElementId>,
    theme: &Theme,
    accent: Hsla,
    modality: &str,
    selected: bool,
) -> Stateful<Div> {
    let color = if selected { accent } else { theme.text_secondary };
    div()
        .id(id)
        .tab_index(0)
        .focus_visible(|style| style.border_color(accent))
        .h(px(26.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(if selected {
            accent.opacity(0.55)
        } else {
            theme.border_strong
        })
        .when(selected, |element| element.bg(accent.opacity(0.14)))
        .when(!selected, |element| {
            element
                .hover(|element| element.bg(theme.overlay))
                .active(|element| element.bg(theme.overlay_strong))
        })
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.0))
        .cursor_default()
        .text_size(ui_px(11.0))
        .text_color(color)
        .children(modality_icon_path(modality).map(|path| icon(path, 11.0, color)))
        .child(SharedString::from(modality_label(modality)))
}

impl Fintwind {
    /// The modality badge for one model id: a quiet pill holding one small
    /// stroke icon per input modality the model accepts, with the spelled-out
    /// modalities in its tooltip. `recorded` is the user's own list, which
    /// wins when present; without one the metadata table answers, and `None`
    /// means the badge waits for a table rather than guessing.
    pub(super) fn model_modality_pill(
        &self,
        theme: &Theme,
        model_id: &str,
        recorded: &[String],
        id: impl Into<ElementId>,
    ) -> Option<AnyElement> {
        let input = if recorded.is_empty() {
            self.models_dev_table
                .as_deref()?
                .resolve_input_modalities(model_id)
        } else {
            recorded.to_vec()
        };
        if input.is_empty() {
            return None;
        }
        let mut glyphs = div().flex().items_center().gap(px(3.0));
        for modality in &input {
            if let Some(path) = modality_icon_path(modality) {
                glyphs = glyphs.child(icon(path, 11.0, theme.text_tertiary));
            }
        }
        Some(
            div()
                .id(id.into())
                .flex_none()
                .tooltip(Tooltip::text(tr!(
                    "providers.modality_tooltip",
                    modalities = modality_labels(&input)
                )))
                .child(
                    div()
                        .px(px(5.0))
                        .py(px(3.0))
                        .rounded_full()
                        .bg(theme.overlay)
                        .flex()
                        .items_center()
                        .child(glyphs),
                )
                .into_any_element(),
        )
    }
}

pub(super) fn info_note(theme: &Theme, icon_path: &'static str, text: String) -> Div {
    div()
        .mt(px(12.0))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(theme.inset)
        .w_full()
        .min_w_0()
        .flex()
        .gap(px(8.0))
        .child(icon(icon_path, 12.0, theme.text_tertiary))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_size(ui_px(10.5))
                .line_height(ui_px(15.0))
                .text_color(theme.text_secondary)
                .child(SharedString::from(text)),
        )
}

pub(super) fn form_hint(theme: &Theme, accent: Hsla, text: impl Into<SharedString>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(icon("icons/alert.svg", 12.0, accent))
        .child(
            div()
                .text_size(ui_px(10.5))
                .text_color(theme.text_secondary)
                .child(text.into()),
        )
        .into_any_element()
}

/// The standard outlined text button, ready for click and key handlers.
pub(super) fn outline_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    icon_path: Option<&'static str>,
    theme: &Theme,
) -> Stateful<Div> {
    let mut button = div()
        .id(id)
        .tab_index(0)
        .focus_visible(|style| style.border_color(providers_accent(theme)))
        .h(px(30.0))
        // Trailing-icon button: the icon side carries the glyph's whitespace,
        // so it takes the smaller share.
        .pl(px(12.0))
        .pr(px(10.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(theme.border_strong)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .cursor_default()
        .text_size(ui_px(12.0))
        .text_color(theme.text_secondary)
        .hover(|element| element.bg(theme.overlay))
        .active(|element| element.bg(theme.overlay_strong))
        .child(label.into());
    if let Some(icon_path) = icon_path {
        button = button.child(icon(icon_path, 12.5, theme.text_tertiary));
    }
    button
}

/// A compact icon-plus-label button for confirm/cancel affordances.
pub(super) fn small_action_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    label: impl Into<SharedString>,
    color: Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .tab_index(0)
        .focus_visible(|style| style.border_color(providers_accent(theme)))
        .h(px(30.0))
        // Leading-icon button: icon side 2px tighter than the text side.
        .pl(px(8.0))
        .pr(px(10.0))
        .rounded(px(7.0))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.0))
        .cursor_default()
        .text_size(ui_px(12.0))
        .text_color(color)
        .hover(|element| element.bg(theme.overlay))
        .active(|element| element.bg(theme.overlay_strong))
        .child(icon(icon_path, 12.5, color))
        .child(label.into())
}

pub(super) fn labeled_field(
    theme: &Theme,
    label: impl Into<SharedString>,
    field: impl IntoElement,
) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .text_size(ui_px(11.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(label.into()),
        )
        .child(field)
}
