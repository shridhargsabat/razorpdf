//! Block-to-Markdown stitching in reading order.
//!
//! Minimal live implementation (fast-path block kinds); heading breadcrumb
//! tracking and VLM-region resolution complete in Phase 4.

use crate::models::{BlockKind, LayoutBlock};

/// Stitch layout blocks into a single Markdown document.
#[must_use]
pub fn blocks_to_markdown(blocks: &[LayoutBlock]) -> String {
    let mut ordered: Vec<&LayoutBlock> = blocks.iter().collect();
    ordered.sort_by_key(|b| b.reading_order);
    let mut out = String::new();
    for b in ordered {
        match &b.kind {
            BlockKind::Heading { level } => {
                let hashes = "#".repeat(usize::from((*level).clamp(1, 6)));
                out.push_str(&format!("{hashes} {}\n\n", b.raw_text.trim()));
            }
            BlockKind::Paragraph => {
                out.push_str(b.raw_text.trim());
                out.push_str("\n\n");
            }
            BlockKind::CodeBlock => {
                out.push_str("```\n");
                out.push_str(&b.raw_text);
                out.push_str("\n```\n\n");
            }
            BlockKind::ListItem { ordered, depth } => {
                let indent = "  ".repeat(usize::from((*depth).min(8)));
                let marker = if *ordered { "1." } else { "-" };
                out.push_str(&format!("{indent}{marker} {}\n", b.raw_text.trim()));
            }
            BlockKind::DeterministicTable { rows, has_header } => {
                let table = crate::geometry::DetectedTable {
                    bbox: b.bbox,
                    rows: rows.clone(),
                    has_header: *has_header,
                };
                out.push_str(&crate::geometry::table_grid::detected_table_to_markdown(
                    &table,
                ));
                out.push('\n');
            }
            BlockKind::VlmFallbackRegion {
                resolved_markdown: Some(md),
                ..
            } => {
                out.push_str(md);
                out.push_str("\n\n");
            }
            BlockKind::VlmFallbackRegion {
                resolved_markdown: None,
                reason,
                ..
            } => {
                out.push_str(&format!("<!-- unresolved VLM region: {reason:?} -->\n\n"));
            }
        }
    }
    out
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
    fn stitches_in_reading_order() {
        let blocks = vec![
            block(1, BlockKind::Paragraph, "Second."),
            block(0, BlockKind::Heading { level: 1 }, "First"),
        ];
        assert_eq!(blocks_to_markdown(&blocks), "# First\n\nSecond.\n\n");
    }
}
