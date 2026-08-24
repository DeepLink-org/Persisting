use dioxus::prelude::*;

use crate::llm::LlmConfig;

#[component]
pub fn LlmSettings(
    config: LlmConfig,
    on_close: EventHandler<MouseEvent>,
    on_save: EventHandler<LlmConfig>,
) -> Element {
    let mut api_base = use_signal(|| config.api_base.clone());
    let mut api_key = use_signal(|| config.api_key.clone());
    let mut model = use_signal(|| config.model.clone());
    rsx! { div { class: "pc2-modal-backdrop high", section { class: "pc2-settings", role: "dialog", aria_modal: "true", header { div { p { class: "eyebrow", "Browser BYOK" } h2 { "Assistant model" } } button { onclick: on_close, "×" } } p { class: "pc2-settings-note", "The key stays in this browser's localStorage. Selected run data is sent directly to this OpenAI-compatible endpoint; pChronicle server never receives the key." } div { class: "pc2-form", label { span { "API base" } input { value: "{api_base}", oninput: move |event| api_base.set(event.value()) } } label { span { "API key" } input { r#type: "password", value: "{api_key}", oninput: move |event| api_key.set(event.value()) } } label { span { "Model" } input { value: "{model}", oninput: move |event| model.set(event.value()) } } } footer { button { class: "button", onclick: on_close, "Cancel" } button { class: "button primary", onclick: move |_| on_save.call(LlmConfig { api_base: api_base(), api_key: api_key(), model: model() }), "Save locally" } } } } }
}
