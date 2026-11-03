//! Markdown & grounded JSON chunk synthesizer.

pub mod chunks;
pub mod columnar;
pub mod diff;
pub mod markdown;

pub use chunks::{count_tokens, estimate_tokens, synthesize_chunks};
pub use columnar::{chunks_to_columnar_batch, chunks_to_jsonl};
pub use diff::{diff_documents, to_markdown_diff};
pub use markdown::blocks_to_markdown;
