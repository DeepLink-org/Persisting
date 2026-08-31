use dioxus::prelude::*;

use crate::catalog_auth::{self, CatalogAuth};
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
    let initial_catalog = catalog_auth::load();
    let mut catalog_access_key = use_signal(|| initial_catalog.access_key.clone());
    let mut catalog_secret_key = use_signal(|| initial_catalog.secret_key.clone());
    rsx! {
        div { class: "pc2-modal-backdrop high",
            section { class: "pc2-settings", role: "dialog", aria_modal: "true",
                header {
                    div { p { class: "eyebrow", "Browser settings" } h2 { "Keys" } }
                    button { onclick: on_close, "×" }
                }
                p { class: "pc2-settings-note",
                    "Catalog keys are sent to this pChronicle server as request headers so it can authorize queries. Assistant keys stay in this browser and are sent only to the OpenAI-compatible endpoint."
                }
                div { class: "pc2-form",
                    p { class: "eyebrow", "Catalog" }
                    label { span { "Access key" } input { value: "{catalog_access_key}", oninput: move |event| catalog_access_key.set(event.value()) } }
                    label { span { "Secret key" } input { r#type: "password", value: "{catalog_secret_key}", oninput: move |event| catalog_secret_key.set(event.value()) } }
                    p { class: "eyebrow", "Assistant model" }
                    label { span { "API base" } input { value: "{api_base}", oninput: move |event| api_base.set(event.value()) } }
                    label { span { "API key" } input { r#type: "password", value: "{api_key}", oninput: move |event| api_key.set(event.value()) } }
                    label { span { "Model" } input { value: "{model}", oninput: move |event| model.set(event.value()) } }
                }
                footer {
                    button { class: "button", onclick: on_close, "Cancel" }
                    button {
                        class: "button primary",
                        onclick: move |_| {
                            catalog_auth::save(&CatalogAuth {
                                access_key: catalog_access_key(),
                                secret_key: catalog_secret_key(),
                            });
                            on_save.call(LlmConfig {
                                api_base: api_base(),
                                api_key: api_key(),
                                model: model(),
                            });
                        },
                        "Save locally"
                    }
                }
            }
        }
    }
}
