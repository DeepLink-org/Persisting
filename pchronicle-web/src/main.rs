#![allow(non_snake_case)]

mod agent;
mod api;
mod chat_view;
mod components;
mod json_value;
mod model;
mod tools;
mod workspace;

fn main() {
    dioxus::prelude::launch(workspace::App);
}
