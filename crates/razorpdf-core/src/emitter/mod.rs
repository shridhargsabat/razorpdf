//! Markdown & grounded JSON chunk synthesizer.

pub mod chunks;
pub mod markdown;

pub use chunks::{count_tokens, estimate_tokens, synthesize_chunks};
pub use markdown::blocks_to_markdown;
