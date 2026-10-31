//! razorpdf-core: ultra-fast local-first document parsing engine.
//!
//! Pipeline: extract → geometry (lines, XY-cut, tables) → triage →
//! VLM fallback (sub-page only) → emit (Markdown + grounded chunks).

pub mod config;
pub mod emitter;
pub mod extractor;
pub mod geometry;
pub mod models;
pub mod test_fixtures;
pub mod triage;
pub mod vlm;

pub use config::{ParserConfig, VlmConfig};
pub use models::*;
