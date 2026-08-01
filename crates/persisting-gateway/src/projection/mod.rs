//! Capture-time human-readable trajectory projection.
//!
//! This layer interprets provider payloads, applies live-only eligibility and
//! history deduplication, and orchestrates AgenticMD upserts. Generic formats,
//! schemas, offline projection, and physical storage remain pChronicle-owned.

pub mod dialogue;
pub mod frontmatter;

#[path = "markdown.rs"]
pub mod markdown_trajectory;

#[path = "pipeline.rs"]
pub mod markdown_pipeline;

#[path = "policy.rs"]
pub mod markdown_policy;

pub mod reconcile;
