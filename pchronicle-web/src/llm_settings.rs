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
    let saved_accounts = catalog_auth::accounts();
    let mut catalog_accounts = use_signal(|| saved_accounts);
    let mut editing = use_signal(|| {
        let accounts = catalog_auth::accounts();
        (!accounts.is_empty()).then(|| {
            accounts
                .iter()
                .position(|a| a == &initial_catalog)
                .unwrap_or(0)
        })
    });
    let mut catalog_label = use_signal(|| initial_catalog.label.clone());
    let mut catalog_access_key = use_signal(|| initial_catalog.access_key.clone());
    let mut catalog_secret_key = use_signal(|| initial_catalog.secret_key.clone());
    let accounts = catalog_accounts();
    let selected_profile = editing()
        .map(|i| i.to_string())
        .unwrap_or_else(|| "__new__".into());
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
                    p { class: "eyebrow", "Catalog identity" }
                    label { span { "Profile" }
                        select { value: "{selected_profile}", onchange: move |event| {
                            let value = event.value();
                            if value == "__new__" { editing.set(None); catalog_label.set(String::new()); catalog_access_key.set(String::new()); catalog_secret_key.set(String::new()); }
                            else if let Ok(index) = value.parse::<usize>() { catalog_auth::select(index); let values = catalog_auth::accounts(); if let Some(auth) = values.get(index) { editing.set(Some(index)); catalog_label.set(auth.label.clone()); catalog_access_key.set(auth.access_key.clone()); catalog_secret_key.set(auth.secret_key.clone()); } }
                        },
                            option { value: "__new__", "＋ New profile" }
                            for (index, auth) in accounts.iter().enumerate() { option { value: "{index}", "{auth.label}" } }
                        }
                    }
                    label { span { "Profile name" } input { placeholder: "e.g. Production", value: "{catalog_label}", oninput: move |event| catalog_label.set(event.value()) } }
                    label { span { "Access key" } input { value: "{catalog_access_key}", oninput: move |event| catalog_access_key.set(event.value()) } }
                    label { span { "Secret key" } input { r#type: "password", value: "{catalog_secret_key}", oninput: move |event| catalog_secret_key.set(event.value()) } }
                    p { class: "eyebrow", "Assistant model" }
                    label { span { "API base" } input { value: "{api_base}", oninput: move |event| api_base.set(event.value()) } }
                    label { span { "API key" } input { r#type: "password", value: "{api_key}", oninput: move |event| api_key.set(event.value()) } }
                    label { span { "Model" } input { value: "{model}", oninput: move |event| model.set(event.value()) } }
                }
                footer {
                    button { class: "button", onclick: on_close, "Cancel" }
                    if editing().is_some() {
                        button { class: "button danger", onclick: move |_| { if let Some(index) = editing() { catalog_auth::remove(index); let values = catalog_auth::accounts(); catalog_accounts.set(values.clone()); if let Some(auth) = values.first() { editing.set(Some(0)); catalog_label.set(auth.label.clone()); catalog_access_key.set(auth.access_key.clone()); catalog_secret_key.set(auth.secret_key.clone()); } else { editing.set(None); catalog_label.set(String::new()); catalog_access_key.set(String::new()); catalog_secret_key.set(String::new()); } } }, "Delete profile" }
                    }
                    button {
                        class: "button primary",
                        onclick: move |_| {
                            let index = catalog_auth::save_at(editing(), &CatalogAuth {
                                label: catalog_label(),
                                access_key: catalog_access_key(),
                                secret_key: catalog_secret_key(),
                            });
                            catalog_accounts.set(catalog_auth::accounts());
                            editing.set(Some(index));
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
