//! Grounded RAG chunk synthesis: one semantic chunk per layout block with
//! heading breadcrumbs, page bboxes, and token counts.

use crate::emitter::markdown::blocks_to_markdown;
use crate::models::{BlockKind, GroundedChunk, LayoutBlock};

/// Synthesize grounded chunks from layout blocks (any order).
///
/// Tracks the heading hierarchy across pages so each chunk carries its
/// `H1 > H2 > H3` ancestor breadcrumb. Blocks rendering to blank Markdown
/// are skipped, so every chunk has non-empty text.
#[must_use]
pub fn synthesize_chunks(blocks: &[LayoutBlock]) -> Vec<GroundedChunk> {
    let mut ordered: Vec<&LayoutBlock> = blocks.iter().collect();
    ordered.sort_by_key(|b| (b.page_index, b.reading_order));

    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut chunks = Vec::new();
    for block in ordered {
        let markdown = blocks_to_markdown(std::slice::from_ref(block));
        if markdown.trim().is_empty() {
            continue;
        }
        let breadcrumb: Vec<String> = stack.iter().map(|(_, t)| t.clone()).collect();
        if let BlockKind::Heading { level } = &block.kind {
            while stack.last().is_some_and(|(l, _)| *l >= *level) {
                stack.pop();
            }
            let ancestors: Vec<String> = stack.iter().map(|(_, t)| t.clone()).collect();
            chunks.push(chunk_for(block, ancestors, markdown));
            stack.push((*level, block.raw_text.trim().to_string()));
        } else {
            chunks.push(chunk_for(block, breadcrumb, markdown));
        }
    }
    chunks
}

fn chunk_for(block: &LayoutBlock, breadcrumb: Vec<String>, markdown: String) -> GroundedChunk {
    GroundedChunk {
        chunk_id: format!("p{}-{:03}", block.page_index, block.reading_order),
        page_index: block.page_index,
        bbox: block.bbox,
        breadcrumb,
        token_estimate: count_tokens(&markdown),
        markdown,
        parsed_by_vlm: matches!(
            &block.kind,
            BlockKind::VlmFallbackRegion {
                resolved_markdown: Some(_),
                ..
            }
        ),
    }
}

/// Token count: accurate tiktoken when the `accurate-tokens` feature is on
/// (falls back to the heuristic offline), else the heuristic.
#[must_use]
pub fn count_tokens(text: &str) -> usize {
    #[cfg(feature = "accurate-tokens")]
    {
        if let Ok(n) = accurate_count(text) {
            return n;
        }
    }
    estimate_tokens(text)
}

/// Heuristic token estimate (~4 chars per token).
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// cl100k token count; fails offline (BPE ranks download on first use).
#[cfg(feature = "accurate-tokens")]
fn accurate_count(text: &str) -> anyhow::Result<usize> {
    let bpe = tiktoken_rs::cl100k_base()?;
    Ok(bpe.encode_with_special_tokens(text).len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BBox;

    fn block(order: usize, kind: BlockKind, text: &str) -> LayoutBlock {
        LayoutBlock {
            id: format!("b{order}"),
            page_index: 0,
            bbox: BBox::new(0.0, 0.0, 10.0, 10.0),
            kind,
            raw_text: text.to_string(),
            reading_order: order,
        }
    }

    #[test]
    fn heuristic_scales_with_length() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn breadcrumbs_track_hierarchy() {
        let blocks = vec![
            block(0, BlockKind::Heading { level: 1 }, "Title"),
            block(1, BlockKind::Heading { level: 3 }, "Section"),
            block(2, BlockKind::Paragraph, "Body text."),
            block(3, BlockKind::Heading { level: 3 }, "Next"),
            block(4, BlockKind::Paragraph, "More."),
        ];
        let chunks = synthesize_chunks(&blocks);
        assert_eq!(chunks.len(), 5);
        assert!(chunks[0].breadcrumb.is_empty());
        assert_eq!(chunks[1].breadcrumb, vec!["Title"]);
        assert_eq!(chunks[2].breadcrumb, vec!["Title", "Section"]);
        assert_eq!(chunks[3].breadcrumb, vec!["Title"]);
        assert_eq!(chunks[4].breadcrumb, vec!["Title", "Next"]);
        assert_eq!(chunks[0].chunk_id, "p0-000");
        assert_eq!(chunks[2].markdown, "Body text.\n\n");
        assert!(!chunks[2].parsed_by_vlm);
    }

    #[test]
    fn blank_blocks_are_skipped() {
        let blocks = vec![
            block(0, BlockKind::Paragraph, "   "),
            block(1, BlockKind::Paragraph, "Real."),
        ];
        let chunks = synthesize_chunks(&blocks);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].markdown, "Real.\n\n");
    }

    #[test]
    fn resolved_vlm_chunks_are_flagged() {
        let blocks = vec![block(
            0,
            BlockKind::VlmFallbackRegion {
                reason: crate::models::TriageReason::OverlappingSpans,
                complexity_score: 0.75,
                resolved_markdown: Some("| a |\n| --- |\n| b |\n".to_string()),
            },
            "raw",
        )];
        let chunks = synthesize_chunks(&blocks);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].parsed_by_vlm);
        assert!(chunks[0].markdown.contains("| a |"));
        assert!(chunks[0].token_estimate > 0);
    }
}
