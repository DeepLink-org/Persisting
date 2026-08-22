#![allow(non_snake_case)]

mod agent;
mod analysis;
mod analysis_agent;
mod analysis_session;
mod api;
mod chat_view;
mod components;
mod json_value;
mod llm;
mod llm_settings;
mod model;
mod result_explorer;
mod result_profile;
mod tools;
mod workspace;

fn main() {
    dioxus::prelude::launch(workspace::App);
}
