//! Markdown & grounded JSON chunk synthesizer.

pub mod chunks;
pub mod markdown;

pub use chunks::estimate_tokens;
pub use markdown::blocks_to_markdown;
