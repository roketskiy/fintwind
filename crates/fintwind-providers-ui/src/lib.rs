//! Shared Provider settings UI primitives, probe error formatters, and
//! subscription quota board state.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use fintwind_input::input::ComposerInput;
use fintwind_opencode_config::custom_providers::{
    self, ApiListError, CustomProviderModel, FirstTokenError, ProviderApiFormat,
};
use fintwind_opencode_config::models_dev::ModelsDevTable;
use fintwind_protocol::provider_quota::ProviderQuota;
use fintwind_protocol::provider_session::{IntegrationFormField, IntegrationOAuthAttempt};
use fintwind_protocol::tr;
use fintwind_ui::theme::{Theme, ui_px};
use fintwind_ui::ui::settings::small_pill;
use fintwind_ui::ui::tooltip::Tooltip;
use fintwind_ui::ui::{icon, motion};
use gpui::{
    AnyElement, App, Div, ElementId, Entity, Hsla, InteractiveElement, ParentElement, SharedString,
    Stateful, Styled, div, prelude::*, px,
};

pub const BUILTIN_SELECTION_PREFIX: &str = "builtin:";

/// Primary accent color on the Providers page.
pub fn providers_accent(theme: &Theme) -> Hsla {
    theme.accent_text
}

/// Foreground color for accent-filled pills/buttons on the Providers page.
pub fn on_providers_accent(theme: &Theme) -> Hsla {
    theme.on_accent
}

/// The selection id for a catalog provider.
pub fn builtin_selection_id(catalog_id: &str) -> String {
    format!("{BUILTIN_SELECTION_PREFIX}{catalog_id}")
}

/// Which detail field a keystroke edit landed in.
#[derive(Clone, Copy)]
pub enum ProviderField {
    BaseUrl,
    ApiKey,
}

/// Which model row the inline model editor is working on.
#[derive(Clone, Debug, PartialEq)]
pub enum ProvidersModelEditor {
    Add,
    Edit(usize),
}

/// Which stage the add-provider form is in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProviderFormStage {
    #[default]
    Picking,
    Custom,
    Builtin(String),
}

/// One in-flight OAuth sign-in.
#[derive(Clone, Debug)]
pub struct ProviderOAuthFlow {
    pub provider_id: String,
    pub attempt: Option<IntegrationOAuthAttempt>,
}

/// One method's pre-start form.
pub struct ProviderOAuthFormDraft {
    pub provider_id: String,
    pub method_id: String,
    pub fields: Vec<IntegrationFormField>,
    pub inputs: Vec<(String, Entity<ComposerInput>)>,
    pub selections: HashMap<String, String>,
}

impl ProviderOAuthFormDraft {
    /// The field's answer so far: the picked option, else the field's text.
    pub fn answer_of(&self, key: &str, cx: &App) -> Option<String> {
        if let Some(selected) = self.selections.get(key) {
            return Some(selected.clone());
        }
        let input = self
            .inputs
            .iter()
            .find(|(field_key, _)| field_key == key)
            .map(|(_, input)| input)?;
        let content = input.read(cx).content().trim().to_owned();
        if content.is_empty() {
            None
        } else {
            Some(content)
        }
    }

    /// Whether the field is answerable: every visibility condition holds
    /// against the answers gathered so far (unanswered references fail).
    pub fn field_is_active(&self, field: &IntegrationFormField, cx: &App) -> bool {
        field.when.iter().all(|condition| {
            let Some(actual) = self.answer_of(&condition.key, cx) else {
                return false;
            };
            let matches = condition.value.as_str() == Some(actual.as_str());
            if condition.op == "neq" {
                !matches
            } else {
                matches
            }
        })
    }

    /// Whether the form can start: every active required field has an answer.
    pub fn ready(&self, cx: &App) -> bool {
        self.fields
            .iter()
            .filter(|field| !field.hidden && field.required)
            .filter(|field| self.field_is_active(field, cx))
            .all(|field| self.answer_of(&field.key, cx).is_some())
    }
}

/// One model draft row of the add-provider form.
pub struct ProviderFormModelDraft {
    pub id: Entity<ComposerInput>,
    pub context: Entity<ComposerInput>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub reasoning_efforts: Vec<String>,
}

pub fn api_format_label(format: ProviderApiFormat) -> String {
    match format {
        ProviderApiFormat::OpenAi => tr!("providers.api_format_openai"),
        ProviderApiFormat::OpenAiResponses => tr!("providers.api_format_openai_responses"),
        ProviderApiFormat::Anthropic => tr!("providers.api_format_anthropic"),
        ProviderApiFormat::Google => tr!("providers.api_format_google"),
    }
}

/// Localized wording for a model-list failure, shared by the fetch action's
/// toast and the connectivity probe's verdict.
pub fn api_list_error_text(error: &ApiListError) -> String {
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
pub fn first_token_error_text(error: &FirstTokenError) -> String {
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

/// Probe latencies read as `812 ms` under a second and `2.41 s` above.
pub fn format_probe_latency(latency: Duration) -> String {
    let millis = latency.as_millis();
    if millis < 1_000 {
        format!("{millis} ms")
    } else {
        format!("{:.2} s", latency.as_secs_f64())
    }
}

pub fn modality_icon_path(modality: &str) -> Option<&'static str> {
    match modality {
        "text" => Some("icons/modality-text.svg"),
        "image" => Some("icons/modality-image.svg"),
        "audio" => Some("icons/modality-audio.svg"),
        "video" => Some("icons/modality-video.svg"),
        "pdf" => Some("icons/modality-pdf.svg"),
        _ => None,
    }
}

pub fn modality_label(modality: &str) -> String {
    match modality {
        "text" => tr!("modality.text"),
        "image" => tr!("modality.image"),
        "audio" => tr!("modality.audio"),
        "video" => tr!("modality.video"),
        "pdf" => tr!("modality.pdf"),
        other => other.to_owned(),
    }
}

pub fn modality_labels(modalities: &[String]) -> String {
    modalities
        .iter()
        .map(|modality| modality_label(modality))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Flip one modality in a selection, keeping the canonical input order.
pub fn toggle_modality(selected: &mut Vec<String>, modality: &str) {
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

/// Flip one output modality in a selection, keeping the canonical output order.
pub fn toggle_output_modality(selected: &mut Vec<String>, modality: &str) {
    if let Some(index) = selected.iter().position(|entry| entry == modality) {
        selected.remove(index);
        return;
    }
    selected.push(modality.to_owned());
    selected.sort_by_key(|entry| {
        custom_providers::OUTPUT_MODALITIES
            .iter()
            .position(|candidate| candidate == entry)
            .unwrap_or(usize::MAX)
    });
}

/// One toggle chip in a modality picker.
pub fn modality_chip(
    id: impl Into<ElementId>,
    theme: &Theme,
    accent: Hsla,
    modality: &str,
    selected: bool,
) -> Stateful<Div> {
    let color = if selected {
        accent
    } else {
        theme.text_secondary
    };
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

/// Modality badge for one model id.
pub fn render_model_modality_pill(
    table: Option<&ModelsDevTable>,
    theme: &Theme,
    model_id: &str,
    recorded: &[String],
    id: impl Into<ElementId>,
) -> Option<AnyElement> {
    let input = if recorded.is_empty() {
        table?.resolve_input_modalities(model_id)
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

/// Image-output badge for one custom model.
pub fn render_model_image_badge(
    table: Option<&ModelsDevTable>,
    theme: &Theme,
    provider_slug: &str,
    model: &CustomProviderModel,
    id: impl Into<ElementId>,
) -> Option<AnyElement> {
    if !fintwind_opencode_config::models_dev::custom_model_emits_image(model, provider_slug, table) {
        return None;
    }
    let accent = providers_accent(theme);
    Some(
        div()
            .id(id.into())
            .flex_none()
            .tooltip(Tooltip::text(tr!("providers.output_modalities_label")))
            .child(small_pill(
                theme,
                tr!("providers.image_badge"),
                Some(accent),
            ))
            .into_any_element(),
    )
}

/// Board display order: local agents first, then coding plans, then
/// pay-as-you-go balances.
pub const SUBSCRIPTION_ORDER: &[&str] = &[
    "anthropic",
    "openai",
    "github-copilot",
    "google",
    "opencode",
    "zhipuai-coding-plan",
    "zai-coding-plan",
    "kimi-code-plan-cn",
    "kimi-code-plan-global",
    "minimax-coding-plan",
    "minimax-cn-coding-plan",
    "commandcode",
    "ollama-cloud",
    "zhipuai",
    "zai",
    "minimax",
    "minimax-cn",
    "openrouter",
    "siliconflow",
    "siliconflow-cn",
    "moonshotai",
    "moonshotai-cn",
    "deepseek",
    "stepfun",
    "stepfun-ai",
    "stepfun-ai-step-plan",
    "stepfun-step-plan",
];

#[derive(Default)]
pub struct SubscriptionEntry {
    pub generation: u64,
    pub loading: bool,
    pub reading: Option<ProviderQuota>,
}

#[derive(Default)]
pub struct SubscriptionBoard {
    pub generation: u64,
    pub refresh: Option<bool>,
    pub connection_error: bool,
    pub connections_loaded: bool,
    pub workspace: Option<(std::path::PathBuf, std::path::PathBuf)>,
    pub entries: BTreeMap<String, SubscriptionEntry>,
    pub refresh_spins: std::cell::RefCell<BTreeMap<String, motion::ActionSpin>>,
}
