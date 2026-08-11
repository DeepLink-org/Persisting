#![allow(non_snake_case)]

mod agent;
mod api;
mod components;
mod model;
mod tools;
mod workspace;

fn main() {
    dioxus::prelude::launch(workspace::App);
}
