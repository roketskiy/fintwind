//! The Providers page's network orchestration: the "fetch models" action,
//! the connectivity probes, and the per-model first-token probes. Split out
//! of `providers_page` so the page file stays about rendering and editing;
//! this is where the page's requests live. Each action spawns its blocking
//! work on the background executor and stores the outcome on the app entity,
//! so render only ever reads finished state.

use crate::theme::ui_px;

use std::time::Duration;

use gpui::KeyDownEvent;

use fintwind_client::custom_providers::{
    self, ApiListError, CustomProvider, CustomProviderModel, FirstTokenError,
};

use super::providers_page::{
    ProviderFormModelDraft, ProviderFormStage, builtin_selection_id, info_note, outline_button,
    small_pill,
};

use super::*;

// ── Probe state ────────────────────────────────────────────────────────────

/// A provider connectivity probe's lifecycle on the page.
#[derive(Clone, Debug)]
pub(super) enum ProviderConnectivityState {
    Testing,
    Done(custom_providers::ConnectivityOutcome),
}

/// A model first-token probe's lifecycle on the page.
#[derive(Clone, Debug)]
pub(super) enum ModelLatencyState {
    Testing,
    Done(Duration),
    Failed(FirstTokenError),
}

impl Fintwind {
    /// The "fetch models" action. The table is reused while fresh, otherwise
    /// downloaded off the UI thread with the session's copy and the disk
    /// cache as offline fallbacks; a table that cannot be had still lets the
    /// API list populate the roster. The provider clicked on is captured up
    /// front — the merge targets it, not whichever provider is selected when
    /// the fetch lands.
    fn fetch_provider_models(&mut self, cx: &mut Context<Self>) {
        if self.models_dev_fetching {
            return;
        }
        let Some(provider) = self.selected_custom_provider() else {
            return;
        };
        let provider_id = provider.id.clone();
        self.models_dev_fetching = true;
        cx.notify();

        let session_table = self.models_dev_table.clone();
        let fresh_table = session_table
            .as_ref()
            .filter(|table| table.age() < fintwind_client::models_dev::TABLE_REUSE_WINDOW)
            .cloned();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    let api_models = custom_providers::fetch_api_model_list(&provider);
                    let table = match fresh_table {
                        Some(table) => Some(table),
                        None => fintwind_client::models_dev::fetch_catalog_with_fallback(
                            session_table.as_deref(),
                        )
                        .map(std::sync::Arc::new),
                    };
                    (api_models, table)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.models_dev_fetching = false;
                let (api_models, table) = fetched;
                match api_models {
                    Ok(api_models) => {
                        if let Some(table) = &table {
                            this.models_dev_table = Some(table.clone());
                        }
                        this.apply_fetched_models(&provider_id, &api_models, table.as_deref(), cx);
                    }
                    Err(error) => {
                        this.show_toast(tr!(
                            "providers.fetch_failed",
                            error = api_list_error_text(&error)
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Merge the fetched API list into `provider_id`'s roster via the
    /// metadata table, commit, and toast the summary. Runs once per fetch
    /// click; render never touches the table or the list.
    fn apply_fetched_models(
        &mut self,
        provider_id: &str,
        api_models: &[fintwind_client::custom_providers::ProviderApiModel],
        table: Option<&fintwind_client::models_dev::ModelsDevTable>,
        cx: &mut Context<Self>,
    ) {
        let outcome = {
            let Some(provider) = self
                .providers_store
                .iter_mut()
                .find(|provider| provider.id == provider_id)
            else {
                return;
            };
            fintwind_client::models_dev::merge_api_models(provider, api_models, table)
        };
        if outcome.added == 0 && outcome.filled == 0 {
            self.show_success_toast(tr!("providers.fetch_up_to_date"));
            return;
        }
        self.commit_custom_providers(cx);
        self.show_success_toast(tr!(
            "providers.fetch_result",
            count = api_models.len(),
            added = outcome.added,
            updated = outcome.filled
        ));
    }

    /// The Models section's header line: the label and, on custom providers,
    /// the models.dev fetch button with its in-flight state.
    pub(super) fn render_models_section_header(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let fetching = self.models_dev_fetching;
        let fetch_button = outline_button(
            "fetch-provider-models",
            if fetching {
                tr!("providers.fetching_models")
            } else {
                tr!("providers.fetch_models")
            },
            Some(if fetching {
                "icons/loader-circle.svg"
            } else {
                "icons/download.svg"
            }),
            theme,
        )
        .opacity(if fetching { 0.6 } else { 1.0 })
        .tooltip(Tooltip::text(tr!("providers.fetch_models_tooltip")))
        .when(!fetching, |element| {
            element
                .on_click(cx.listener(|this, _, _, cx| {
                    this.fetch_provider_models(cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.fetch_provider_models(cx);
                        cx.stop_propagation();
                    }
                }))
        });

        models_header_with_button(theme, tr!("providers.models_label"), fetch_button)
    }

    /// The add form's Models section header: the label and the form's fetch
    /// button. The button answers a click even with an unusable Base URL —
    /// the action explains itself with a toast — so it stays operable.
    pub(super) fn render_form_models_section_header(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let fetching = self.providers_form_fetching;
        let fetch_button = outline_button(
            "fetch-form-provider-models",
            if fetching {
                tr!("providers.fetching_models")
            } else {
                tr!("providers.fetch_models")
            },
            Some(if fetching {
                "icons/loader-circle.svg"
            } else {
                "icons/download.svg"
            }),
            theme,
        )
        .opacity(if fetching { 0.6 } else { 1.0 })
        .tooltip(Tooltip::text(tr!("providers.fetch_models_tooltip")))
        .when(!fetching, |element| {
            element
                .on_click(cx.listener(|this, _, _, cx| {
                    this.fetch_form_models(cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.fetch_form_models(cx);
                        cx.stop_propagation();
                    }
                }))
        });

        models_header_with_button(theme, tr!("providers.models_label"), fetch_button)
    }

    // ── Connectivity & first-token probes ──────────────────────────────────

    /// Make sure a metadata table is available for the page's modality
    /// badges: the session's copy while fresh, else the disk cache, else one
    /// background download; a stale copy is shown at once while a refresh is
    /// attempted. Silent on failure — the badges simply wait for a fetch,
    /// and the explicit fetch button remains the user's lever.
    pub(super) fn ensure_models_dev_table(&mut self, cx: &mut Context<Self>) {
        if self
            .models_dev_table
            .as_ref()
            .is_some_and(|table| table.age() < fintwind_client::models_dev::TABLE_REUSE_WINDOW)
            || self.models_dev_loading
        {
            return;
        }
        self.models_dev_loading = true;
        let session_table = self.models_dev_table.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let cached = fintwind_client::models_dev::cached_catalog();
                    let best = match (&session_table, cached) {
                        (Some(session), Some(cached)) => {
                            Some(if session.fetched_at >= cached.fetched_at {
                                (**session).clone()
                            } else {
                                cached
                            })
                        }
                        (Some(session), None) => Some((**session).clone()),
                        (None, cached) => cached,
                    };
                    match best {
                        Some(table)
                            if table.age() < fintwind_client::models_dev::TABLE_REUSE_WINDOW =>
                        {
                            Some(table)
                        }
                        // Stale or missing: go to the network; any known copy
                        // is the offline fallback.
                        best => fintwind_client::models_dev::fetch_catalog().ok().or(best),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.models_dev_loading = false;
                if let Some(table) = loaded {
                    this.models_dev_table = Some(std::sync::Arc::new(table));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The connectivity probe: one model-list request through the provider's
    /// endpoint with a short deadline, timed end to end. The provider under
    /// the selection when the button was clicked is the one probed.
    pub(super) fn probe_provider_connectivity(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.selected_custom_provider() else {
            return;
        };
        if matches!(
            self.provider_connectivity.get(&provider.id),
            Some(ProviderConnectivityState::Testing)
        ) {
            return;
        }
        let provider_id = provider.id.clone();
        self.provider_connectivity
            .insert(provider_id.clone(), ProviderConnectivityState::Testing);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { custom_providers::probe_connectivity(&provider) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.provider_connectivity
                    .insert(provider_id, ProviderConnectivityState::Done(outcome));
                cx.notify();
            });
        })
        .detach();
    }

    // ── The add-provider form's actions ────────────────────────────────────
    //
    // The form describes a provider that does not exist yet, so its fetch
    // and probe run against a transient provider built from the fields at
    // click time, and the results land on the form, not the roster.

    /// The add form's fields as a transient provider: exactly what the two
    /// form requests need. The models are the drafts as entered, so a fetch
    /// keeps and fills hand-entered ids the same way the saved roster's does.
    fn form_transient_provider(&self, cx: &App) -> CustomProvider {
        CustomProvider::new(
            String::new(),
            self.provider_form_name.read(cx).content().trim().to_owned(),
            self.provider_form_base_url
                .read(cx)
                .content()
                .trim()
                .to_owned(),
            self.providers_form_format,
            self.provider_form_api_key
                .read(cx)
                .content()
                .trim()
                .to_owned(),
            self.form_draft_models(cx),
        )
    }

    /// The add form's draft rows as models, skipping empty ids — the same
    /// collection the submit uses.
    fn form_draft_models(&self, cx: &App) -> Vec<CustomProviderModel> {
        let mut models = Vec::new();
        for draft in &self.providers_form_models {
            let id = draft.id.read(cx).content().trim().to_owned();
            if id.is_empty() {
                continue;
            }
            let context = draft.context.read(cx).content();
            models.push(CustomProviderModel {
                id,
                context_window: custom_providers::parse_context_window(context),
                input_modalities: draft.input_modalities.clone(),
                ..Default::default()
            });
        }
        models
    }

    /// The add form's "fetch models": the saved provider's request driven by
    /// a transient provider, with the merged result becoming draft rows. The
    /// request also runs while the provider has no models yet — the form's
    /// whole point is to fill that list.
    pub(super) fn fetch_form_models(&mut self, cx: &mut Context<Self>) {
        if self.providers_form_fetching {
            return;
        }
        if !self.form_endpoint_ready(cx) {
            self.show_toast(tr!("providers.error_invalid_url"));
            return;
        }
        let provider = self.form_transient_provider(cx);
        self.providers_form_fetching = true;
        self.providers_form_fetch_generation += 1;
        let generation = self.providers_form_fetch_generation;
        cx.notify();

        let session_table = self.models_dev_table.clone();
        let fresh_table = session_table
            .as_ref()
            .filter(|table| table.age() < fintwind_client::models_dev::TABLE_REUSE_WINDOW)
            .cloned();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    let api_models = custom_providers::fetch_api_model_list(&provider);
                    let table = match fresh_table {
                        Some(table) => Some(table),
                        None => fintwind_client::models_dev::fetch_catalog_with_fallback(
                            session_table.as_deref(),
                        )
                        .map(std::sync::Arc::new),
                    };
                    (api_models, table)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.providers_form_fetching = false;
                if this.providers_form_fetch_generation != generation {
                    // The form was left and reopened while the request ran;
                    // its result no longer describes these fields.
                    cx.notify();
                    return;
                }
                let (api_models, table) = fetched;
                match api_models {
                    Ok(api_models) => {
                        if let Some(table) = &table {
                            this.models_dev_table = Some(table.clone());
                        }
                        this.apply_fetched_form_models(&api_models, table.as_deref(), window, cx);
                    }
                    Err(error) => {
                        this.show_toast(tr!(
                            "providers.fetch_failed",
                            error = api_list_error_text(&error)
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Merge the fetched API list into the form's drafts via the metadata
    /// table, rebuild the draft rows, and toast the summary. Runs once per
    /// fetch click, with the window needed to mint draft fields.
    fn apply_fetched_form_models(
        &mut self,
        api_models: &[fintwind_client::custom_providers::ProviderApiModel],
        table: Option<&fintwind_client::models_dev::ModelsDevTable>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut merged = self.form_transient_provider(cx);
        let outcome = fintwind_client::models_dev::merge_api_models(&mut merged, api_models, table);
        if outcome.added == 0 && outcome.filled == 0 {
            self.show_success_toast(tr!("providers.fetch_up_to_date"));
            return;
        }
        // The fields can hold only an id and a context window; the rest of
        // what the merge learned waits here for the submit.
        self.providers_form_model_catalog = merged
            .models
            .iter()
            .map(|model| (model.id.clone(), model.clone()))
            .collect();
        self.rebuild_form_model_rows(&merged.models, window, cx);
        self.show_success_toast(tr!(
            "providers.fetch_result",
            count = api_models.len(),
            added = outcome.added,
            updated = outcome.filled
        ));
    }

    /// Replace the add form's draft rows with one per model: the id typed
    /// back in and the context window as formatted text. Called from the
    /// fetch's landing, so the fields must be minted here, not in render.
    fn rebuild_form_model_rows(
        &mut self,
        models: &[CustomProviderModel],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.providers_form_models.clear();
        for model in models {
            let id = cx.new(|cx| {
                ComposerInput::new(window, cx)
                    .search_field()
                    .placeholder(tr!("providers.model_id_placeholder"))
            });
            id.update(cx, |input, cx| input.set_content(model.id.clone(), cx));
            let context = cx.new(|cx| {
                ComposerInput::new(window, cx)
                    .search_field()
                    .placeholder(tr!("providers.context_window_placeholder"))
            });
            if let Some(tokens) = model.context_window {
                let text = custom_providers::format_context_window(tokens);
                context.update(cx, |input, cx| input.set_content(text, cx));
            }
            // Only what the draft itself recorded: the catalog's modalities
            // stay a display fallback and must not freeze into the config on
            // a fetch-plus-submit the user never toggled.
            let input_modalities = model.input_modalities.clone();
            self.providers_form_models.push(ProviderFormModelDraft {
                id,
                context,
                input_modalities,
            });
        }
        cx.notify();
    }

    /// The add form's connectivity probe: the saved provider's request
    /// against the transient provider, with the verdict on the form.
    pub(super) fn probe_form_connectivity(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.providers_form_connectivity,
            Some(ProviderConnectivityState::Testing)
        ) {
            return;
        }
        if !self.form_endpoint_ready(cx) {
            self.show_toast(tr!("providers.error_invalid_url"));
            return;
        }
        let provider = self.form_transient_provider(cx);
        self.providers_form_probe_generation += 1;
        let generation = self.providers_form_probe_generation;
        self.providers_form_connectivity = Some(ProviderConnectivityState::Testing);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { custom_providers::probe_connectivity(&provider) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.providers_form_probe_generation == generation {
                    this.providers_form_connectivity =
                        Some(ProviderConnectivityState::Done(outcome));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether the add form's Base URL is one the requests can run against;
    /// gates the form's two buttons.
    fn form_endpoint_ready(&self, cx: &App) -> bool {
        custom_providers::base_url_valid(self.provider_form_base_url.read(cx).content().trim())
    }

    /// Leave the add form: its probe state is void — the fields it described
    /// are going away with it. The fetch flag is left alone so it keeps
    /// telling the truth about an in-flight request.
    pub(super) fn exit_provider_form(&mut self, cx: &mut Context<Self>) {
        self.providers_adding = false;
        self.providers_form_stage = ProviderFormStage::Picking;
        self.providers_form_connectivity = None;
        self.providers_form_model_catalog.clear();
        self.providers_form_fetch_generation += 1;
        self.providers_form_probe_generation += 1;
        cx.notify();
    }

    /// The first-token probe for one model: a minimal streaming chat request
    /// timed to its first token. Keyed by (provider id, model id), so a
    /// result follows the model rather than the row it sat in.
    pub(super) fn test_model_first_token(
        &mut self,
        provider_id: String,
        model_id: String,
        cx: &mut Context<Self>,
    ) {
        let key = (provider_id.clone(), model_id.clone());
        if matches!(
            self.model_latency.get(&key),
            Some(ModelLatencyState::Testing)
        ) {
            return;
        }
        let Some(provider) = self
            .providers_store
            .iter()
            .find(|provider| provider.id == provider_id)
            .cloned()
        else {
            return;
        };
        self.model_latency.insert(key, ModelLatencyState::Testing);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let probe_model_id = model_id.clone();
            let result =
                cx.background_executor()
                    .spawn(async move {
                        custom_providers::first_token_latency(&provider, &probe_model_id)
                    })
                    .await;
            let _ = this.update(cx, |this, cx| {
                let key = (provider_id, model_id);
                this.model_latency.insert(
                    key,
                    match result {
                        Ok(latency) => ModelLatencyState::Done(latency),
                        Err(error) => ModelLatencyState::Failed(error),
                    },
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// A provider's probes stop being current the moment its endpoint or key
    /// changes; drop them so the UI shows "unknown" rather than a stale
    /// verdict.
    pub(super) fn clear_provider_probe_results(&mut self, provider_id: &str) {
        self.provider_connectivity.remove(provider_id);
        self.model_latency
            .retain(|(owner, _), _| owner != provider_id);
    }

    // ── Built-in providers: catalog, auth state, authorize & logout ────────

    /// Make sure the built-in provider roster and the authorized set are
    /// loaded for the page visit. Both run off the UI thread; render reads
    /// only what they store and degrades to "unknown" until then.
    pub(super) fn ensure_builtin_providers(&mut self, cx: &mut Context<Self>) {
        self.ensure_models_dev_table(cx);
        if self.providers_builtin.is_none() && !self.providers_builtin_loading {
            self.providers_builtin_loading = true;
            let session = self.providers_builtin.clone();
            cx.spawn(async move |this, cx| {
                let loaded = cx
                    .background_executor()
                    .spawn(async move {
                        fintwind_client::models_dev::fetch_providers_with_fallback(
                            session.as_deref(),
                        )
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.providers_builtin_loading = false;
                    if let Ok(providers) = loaded {
                        this.providers_builtin = Some(std::sync::Arc::new(providers));
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        if !self.providers_auth_loading {
            self.providers_auth_loading = true;
            let workspace = self.provider_workspace();
            let daemon = self.daemon.clone();
            cx.spawn(async move |this, cx| {
                let integrations = cx
                    .background_executor()
                    .spawn(async move {
                        workspace
                            .map(|(binary, directory)| {
                                fintwind_client::persistence::StateStore::remote(daemon)
                                    .fetch_integrations(binary, directory)
                            })
                            .unwrap_or_else(|| {
                                Err(std::io::Error::other("no OpenCode server to ask"))
                            })
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.providers_auth_loading = false;
                    match integrations {
                        Ok(integrations) => this.apply_integrations(integrations),
                        Err(error) => this.show_toast(tr!(
                            "providers.auth_load_failed",
                            error = error.to_string()
                        )),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Store one integration fetch: the authorized set and the key-method
    /// set. A integration row whose id the catalog does not know still
    /// counts as authorized — the roster must not hide a working credential.
    fn apply_integrations(
        &mut self,
        integrations: Vec<fintwind_protocol::provider_session::IntegrationSummary>,
    ) {
        self.providers_authorized = integrations
            .iter()
            .filter(|integration| integration.connected)
            .map(|integration| integration.id.clone())
            .collect();
        self.providers_key_methods = integrations
            .iter()
            .filter(|integration| integration.supports_key)
            .map(|integration| integration.id.clone())
            .collect();
    }

    /// The workspace the page's server RPCs anchor to: the probed OpenCode
    /// binary and a project checkout. The credential store is shared by
    /// every server of the same OpenCode install, so the directory only
    /// picks which pool entry answers.
    fn provider_workspace(&self) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let binary = self.native_binary_path()?;
        let directory = self
            .state
            .selected_project
            .and_then(|id| self.state.projects.iter().find(|project| project.id == id))
            .or_else(|| self.state.projects.first())
            .map(|project| project.path.clone())?;
        Some((binary, directory))
    }

    /// The built-in provider record for `id`, `None` when the catalog has
    /// not loaded (or does not know the id).
    pub(super) fn builtin_provider(
        &self,
        id: &str,
    ) -> Option<&fintwind_client::models_dev::ModelsDevProvider> {
        self.providers_builtin
            .as_deref()
            .and_then(|roster| roster.get(id))
    }

    /// Connect the key the add form holds for `provider_id` through the
    /// workspace server's connect API — the same path the TUI's /connect
    /// takes. The running server adopts the credential immediately and
    /// persists it in its own store, so nothing needs a restart. The refresh
    /// of the authorized set lands when the connect did.
    pub(super) fn authorize_builtin_provider(
        &mut self,
        provider_id: String,
        cx: &mut Context<Self>,
    ) {
        let key = self
            .provider_form_api_key
            .read(cx)
            .content()
            .trim()
            .to_owned();
        if key.is_empty() {
            return;
        }
        let Some(workspace) = self.provider_workspace() else {
            return;
        };
        self.providers_auth_loading = true;
        let daemon = self.daemon.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let (binary, directory) = workspace;
                    let store = fintwind_client::persistence::StateStore::remote(daemon);
                    store
                        .authorize_provider(
                            binary.clone(),
                            directory.clone(),
                            provider_id.clone(),
                            key,
                        )
                        .map(|_| provider_id)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.providers_auth_loading = false;
                match outcome {
                    Ok(provider_id) => {
                        // The server is the truth: re-read its connection
                        // list instead of assuming the connect took.
                        this.providers_authorized.insert(provider_id.clone());
                        this.providers_key_methods.insert(provider_id.clone());
                        this.exit_provider_form(cx);
                        this.select_provider(builtin_selection_id(&provider_id), cx);
                        this.show_success_toast(tr!("providers.authorized_toast"));
                        this.refresh_integrations(cx);
                    }
                    Err(error) => this
                        .show_toast(tr!("providers.auth_save_failed", error = error.to_string())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Remove `provider_id`'s connected credential from the server — the
    /// logout. The page then shows the provider as unauthorized; its stale
    /// probe verdict is dropped.
    pub(super) fn logout_builtin_provider(&mut self, provider_id: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.provider_workspace() else {
            return;
        };
        self.providers_auth_loading = true;
        self.providers_delete_arming = None;
        let daemon = self.daemon.clone();
        cx.spawn(async move |this, cx| {
            let removed = cx
                .background_executor()
                .spawn(async move {
                    let (binary, directory) = workspace;
                    fintwind_client::persistence::StateStore::remote(daemon)
                        .logout_provider(binary, directory, provider_id.clone())
                        .map(|_| provider_id)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.providers_auth_loading = false;
                match removed {
                    Ok(provider_id) => {
                        this.providers_authorized.remove(provider_id.as_str());
                        if this.providers_builtin_probe_id.as_deref() == Some(provider_id.as_str())
                        {
                            this.providers_builtin_connectivity = None;
                            this.providers_builtin_probe_id = None;
                        }
                        this.show_success_toast(tr!("providers.logged_out_toast"));
                        this.refresh_integrations(cx);
                    }
                    Err(error) => this
                        .show_toast(tr!("providers.auth_save_failed", error = error.to_string())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-fetch the integration list in the background, so the authorized
    /// set mirrors the server rather than what this action assumed. Failure
    /// is quiet — the optimistic set from the action stands.
    fn refresh_integrations(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.provider_workspace() else {
            return;
        };
        let daemon = self.daemon.clone();
        cx.spawn(async move |this, cx| {
            let integrations = cx
                .background_executor()
                .spawn(async move {
                    let (binary, directory) = workspace;
                    fintwind_client::persistence::StateStore::remote(daemon)
                        .fetch_integrations(binary, directory)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(integrations) = integrations {
                    this.apply_integrations(integrations);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The selected built-in provider's connectivity probe: how many models
    /// the workspace server currently exposes for it, timed. The server
    /// lists a provider's models only when it holds a credential it
    /// accepts, so the count is the verdict — and the key never leaves the
    /// server, which no longer holds it in a file the app could read.
    pub(super) fn probe_builtin_provider_connectivity(&mut self, cx: &mut Context<Self>) {
        let Some(catalog_id) = self.selected_builtin_catalog_id().map(str::to_owned) else {
            return;
        };
        // Per-provider in-flight guard: a probe running for another provider
        // must not mute this one's button, and a second click here must not
        // stack a duplicate request.
        if matches!(
            self.providers_builtin_connectivity,
            Some(ProviderConnectivityState::Testing)
        ) && self.providers_builtin_probe_id.as_deref() == Some(catalog_id.as_str())
        {
            return;
        }
        // The probe asks the server about a credential it holds; without one
        // there is nothing to test.
        if !self.provider_is_authorized(&catalog_id) {
            return;
        }
        let Some(workspace) = self.provider_workspace() else {
            return;
        };
        self.providers_builtin_connectivity = Some(ProviderConnectivityState::Testing);
        self.providers_builtin_probe_id = Some(catalog_id.clone());
        cx.notify();
        let probe_id = catalog_id.clone();
        let daemon = self.daemon.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let (binary, directory) = workspace;
                    let probed = fintwind_client::persistence::StateStore::remote(daemon)
                        .probe_builtin_provider(binary, directory, probe_id.clone());
                    match probed {
                        Ok((models, latency)) => {
                            if models > 0 {
                                custom_providers::ConnectivityOutcome::Reachable { models, latency }
                            } else {
                                // The server answered but exposes no model
                                // for the provider: its credential is
                                // absent or rejected.
                                custom_providers::ConnectivityOutcome::Failed {
                                    error: custom_providers::ApiListError::AuthRejected(401),
                                }
                            }
                        }
                        Err(error) => custom_providers::ConnectivityOutcome::Failed {
                            error: custom_providers::ApiListError::Unreachable(error.to_string()),
                        },
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.providers_builtin_probe_id.as_deref() == Some(catalog_id.as_str()) {
                    this.providers_builtin_connectivity =
                        Some(ProviderConnectivityState::Done(outcome));
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Fintwind {
    /// The built-in provider page's connectivity row: the test button beside
    /// its verdict. The probe asks the workspace's OpenCode server how many
    /// models it exposes for the provider — a credential the server accepts
    /// is what makes them appear; providers without a key method (OAuth-
    /// gated) show a note instead of a button.
    pub(super) fn render_builtin_connectivity_field(
        &self,
        state: Option<&ProviderConnectivityState>,
        provider: &fintwind_client::models_dev::ModelsDevProvider,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let testing = matches!(state, Some(ProviderConnectivityState::Testing));
        if !self.provider_supports_key(&provider.id) {
            return info_note(
                theme,
                "icons/info.svg",
                tr!("providers.builtin_no_endpoint"),
            )
            .into_any_element();
        }
        let mut button = outline_button(
            "test-builtin-provider-connection",
            if testing {
                tr!("providers.testing_connection")
            } else {
                tr!("providers.test_connection")
            },
            Some(if testing {
                "icons/loader-circle.svg"
            } else {
                "icons/globe.svg"
            }),
            theme,
        )
        .tooltip(Tooltip::text(tr!("providers.connection_tooltip")));
        if testing {
            button = button.opacity(0.6);
        } else {
            button = button
                .on_click(cx.listener(|this, _, _, cx| {
                    this.probe_builtin_provider_connectivity(cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.probe_builtin_provider_connectivity(cx);
                        cx.stop_propagation();
                    }
                }));
        }

        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(button)
            .children(connectivity_verdict(state, theme))
            .into_any_element()
    }
}

/// A Models section's header line: the label left, the given button right.
/// Both the saved provider's and the add form's headers share the layout.
fn models_header_with_button(theme: &Theme, label: String, button: Stateful<Div>) -> Div {
    div()
        .flex()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pt(px(2.0))
                .pb(px(4.0))
                .px(px(9.0))
                .flex()
                .items_baseline()
                .text_size(ui_px(9.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_tertiary)
                .child(SharedString::from(label.to_uppercase())),
        )
        .child(button)
}

/// Localized wording for a model-list failure, shared by the fetch action's
/// toast and the connectivity probe's verdict.
pub(super) fn api_list_error_text(error: &ApiListError) -> String {
    match error {
        ApiListError::InvalidBaseUrl => tr!("providers.error_invalid_url"),
        ApiListError::AuthRejected(status) => {
            tr!("providers.error_auth", status = status.to_string())
        }
        ApiListError::ListMissing => tr!("providers.error_list_missing"),
        ApiListError::HttpStatus(status) => {
            tr!("providers.error_status", status = status.to_string())
        }
        ApiListError::Unreachable(error) => {
            tr!("providers.error_unreachable", error = error.clone())
        }
        ApiListError::NoModelList(error) => {
            tr!("providers.error_no_list", error = error.clone())
        }
    }
}

/// Localized wording for a first-token probe failure; the provider's own
/// error sentence, when it sent one, is appended for the tooltip.
pub(super) fn first_token_error_text(error: &FirstTokenError) -> String {
    match error {
        FirstTokenError::InvalidBaseUrl => tr!("providers.error_invalid_url"),
        FirstTokenError::HttpStatus { status, message } => {
            let wording = match status {
                401 | 403 => tr!("providers.ttft_error_auth", status = status.to_string()),
                404 => tr!("providers.ttft_error_model"),
                status => tr!("providers.error_status", status = status.to_string()),
            };
            append_detail(&wording, message.as_deref())
        }
        FirstTokenError::Unreachable(error) => {
            tr!("providers.error_unreachable", error = error.clone())
        }
        FirstTokenError::NoStreamData { message } => {
            append_detail(&tr!("providers.ttft_error_no_stream"), message.as_deref())
        }
        FirstTokenError::Timeout => tr!(
            "providers.ttft_error_timeout",
            seconds = custom_providers::FIRST_TOKEN_TIMEOUT_SECS.to_string()
        ),
    }
}

fn append_detail(wording: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => format!("{wording} · {detail}"),
        _ => wording.to_owned(),
    }
}

/// Probe latencies read as `812 ms` under a second and `2.41 s` above —
/// milliseconds stop being readable in the thousands.
pub(super) fn format_probe_latency(latency: Duration) -> String {
    let millis = latency.as_millis();
    if millis < 1_000 {
        format!("{millis} ms")
    } else {
        format!("{:.2} s", latency.as_secs_f64())
    }
}

impl Fintwind {
    /// The endpoint section's connectivity row: the test button beside its
    /// verdict — spinner while probing, a reachable line with the measured
    /// latency, or the failure reason in the danger color.
    pub(super) fn render_connectivity_field(
        &self,
        provider: &fintwind_client::custom_providers::CustomProvider,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.provider_connectivity.get(&provider.id);
        let testing = matches!(state, Some(ProviderConnectivityState::Testing));
        let mut button = outline_button(
            "test-provider-connection",
            if testing {
                tr!("providers.testing_connection")
            } else {
                tr!("providers.test_connection")
            },
            Some(if testing {
                "icons/loader-circle.svg"
            } else {
                "icons/globe.svg"
            }),
            theme,
        )
        .tooltip(Tooltip::text(tr!("providers.connection_tooltip")));
        if testing {
            button = button.opacity(0.6);
        } else {
            button = button
                .on_click(cx.listener(|this, _, _, cx| {
                    this.probe_provider_connectivity(cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.probe_provider_connectivity(cx);
                        cx.stop_propagation();
                    }
                }));
        }

        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(button)
            .children(connectivity_verdict(state, theme))
            .into_any_element()
    }

    /// The add form's connectivity row: the same test for the provider the
    /// form describes, which does not exist yet, so the verdict lives on the
    /// form. Like the form's fetch button, it stays clickable with an
    /// unusable Base URL — the action explains itself with a toast.
    pub(super) fn render_form_connectivity_field(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.providers_form_connectivity.as_ref();
        let testing = matches!(state, Some(ProviderConnectivityState::Testing));
        let mut button = outline_button(
            "test-provider-form-connection",
            if testing {
                tr!("providers.testing_connection")
            } else {
                tr!("providers.test_connection")
            },
            Some(if testing {
                "icons/loader-circle.svg"
            } else {
                "icons/globe.svg"
            }),
            theme,
        )
        .tooltip(Tooltip::text(tr!("providers.connection_tooltip")));
        if testing {
            button = button.opacity(0.6);
        } else {
            button = button
                .on_click(cx.listener(|this, _, _, cx| {
                    this.probe_form_connectivity(cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if !event.keystroke.modifiers.modified()
                        && matches!(event.keystroke.key.as_str(), "enter" | "space")
                    {
                        this.probe_form_connectivity(cx);
                        cx.stop_propagation();
                    }
                }));
        }

        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(button)
            .children(connectivity_verdict(state, theme))
            .into_any_element()
    }
}

/// A connectivity probe's verdict: a reachable line with the measured
/// latency, or the failure reason. `None` while unknown or probing.
fn connectivity_verdict(
    state: Option<&ProviderConnectivityState>,
    theme: &Theme,
) -> Option<AnyElement> {
    match state? {
        ProviderConnectivityState::Testing => None,
        ProviderConnectivityState::Done(custom_providers::ConnectivityOutcome::Reachable {
            models,
            latency,
        }) => Some(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .min_w_0()
                .child(icon("icons/check.svg", 12.0, theme.success))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(ui_px(10.5))
                        .text_color(theme.success)
                        .child(SharedString::from(tr!(
                            "providers.connection_ok",
                            latency = format_probe_latency(*latency),
                            count = *models
                        ))),
                )
                .into_any_element(),
        ),
        ProviderConnectivityState::Done(custom_providers::ConnectivityOutcome::Failed {
            error,
        }) => {
            let text = api_list_error_text(error);
            Some(
                div()
                    .id("provider-connectivity-verdict")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .min_w_0()
                    .tooltip(Tooltip::text(text.clone()))
                    .child(icon("icons/alert.svg", 12.0, theme.danger))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_px(10.5))
                            .text_color(theme.danger)
                            .child(SharedString::from(tr!(
                                "providers.connection_failed",
                                error = text
                            ))),
                    )
                    .into_any_element(),
            )
        }
    }
}

impl Fintwind {
    /// A model row's first-token probe: the result (or spinner) beside the
    /// zap button that (re-)runs it.
    pub(super) fn render_model_latency_cluster(
        &self,
        provider_id: &str,
        model_id: &str,
        index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (provider_id.to_owned(), model_id.to_owned());
        let state = self.model_latency.get(&key);
        let testing = matches!(state, Some(ModelLatencyState::Testing));
        let mut cluster = div().flex().items_center().gap(px(4.0)).flex_none();
        match state {
            Some(ModelLatencyState::Testing) => {
                // The zap button below spins while the probe runs; nothing
                // else is added so the row shows exactly one spinner.
            }
            Some(ModelLatencyState::Done(latency)) => {
                let text = format_probe_latency(*latency);
                cluster = cluster.child(
                    div()
                        .id(SharedString::from(format!("ttft-result-{index}")))
                        .flex_none()
                        .tooltip(Tooltip::text(tr!(
                            "providers.ttft_result",
                            latency = text.clone()
                        )))
                        .child(small_pill(theme, text, Some(theme.success))),
                );
            }
            Some(ModelLatencyState::Failed(error)) => {
                let text = first_token_error_text(error);
                cluster = cluster.child(
                    div()
                        .id(SharedString::from(format!("ttft-result-{index}")))
                        .flex_none()
                        .tooltip(Tooltip::text(text))
                        .child(small_pill(
                            theme,
                            tr!("providers.ttft_failed"),
                            Some(theme.danger),
                        )),
                );
            }
            None => {}
        }
        let zap = icon_button(
            SharedString::from(format!("ttft-model-{index}")),
            "icons/zap.svg",
            *theme,
        )
        .tooltip(Tooltip::text(tr!("providers.latency_test_tooltip")));
        let provider_id = provider_id.to_owned();
        let model_id = model_id.to_owned();
        cluster
            .child(if testing {
                // The probe is already running; the button gives way to the
                // spinner so a second click cannot stack a duplicate request.
                div()
                    .id(SharedString::from(format!("ttft-model-{index}")))
                    .size(px(26.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(motion::spin_slow(icon(
                        "icons/zap.svg",
                        14.0,
                        theme.text_tertiary,
                    )))
                    .into_any_element()
            } else {
                zap.on_click(cx.listener({
                    let (provider_id, model_id) = (provider_id.clone(), model_id.clone());
                    move |this, _, _, cx| {
                        this.test_model_first_token(provider_id.clone(), model_id.clone(), cx);
                    }
                }))
                .on_key_down(cx.listener({
                    let (provider_id, model_id) = (provider_id.clone(), model_id.clone());
                    move |this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified()
                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                        {
                            this.test_model_first_token(provider_id.clone(), model_id.clone(), cx);
                            cx.stop_propagation();
                        }
                    }
                }))
                .into_any_element()
            })
            .into_any_element()
    }
}
