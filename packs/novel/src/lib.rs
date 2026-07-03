#![allow(clippy::type_complexity)]

//! novel-pack: a domain pack for long-form novel authoring.
//!
//! Builds a multi-node workflow on top of `lim-workflow` that decomposes a
//! story bible into scenes, drafts each scene via an LLM, reviews them, and
//! assembles a final chapter. Provides `StoryBible` / `SceneOutline` /
//! `ChapterOutline` types and [`build_chapter_workflow`].

pub mod editor;
pub mod orchestrator;
pub mod reviewer;
pub mod types;
pub mod workflow;
pub mod writer;

pub use types::{ChapterOutline, Character, ReviewReport, SceneOutline, StoryBible};
pub use workflow::build_chapter_workflow;
