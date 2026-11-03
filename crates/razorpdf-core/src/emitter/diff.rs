//! Structural PDF diff engine: section/paragraph/table-cell redlines.
//!
//! [`diff_documents`] aligns chunks by `(section_path, block_kind)` and
//! token Jaccard similarity, classifying each block as Unchanged,
//! Modified, Added, or Removed with bounding boxes on both revisions.

use std::collections::{BTreeMap, HashSet};

use crate::models::{BlockDiff, DiffStatus, DocumentDiff, GroundedChunk, ParsedDocument};

/// Minimum Jaccard similarity for a Modified match (else Added/Removed).
const MODIFIED_MIN_SIM: f32 = 0.35;
/// Similarity at/above which normalized-identical text is Unchanged.
const UNCHANGED_SIM: f32 = 0.999;
/// Max chars of old/new text kept in the Markdown redline.
const REDLINE_TEXT_LIMIT: usize = 400;

/// Compare two revisions into a structured diff.
pub fn diff_documents(old: &ParsedDocument, new: &ParsedDocument) -> DocumentDiff {
    // New-chunk index per alignment key; greedy best-similarity match.
    let mut index: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (i, chunk) in new.chunks.iter().enumerate() {
        index.entry(chunk_key(chunk)).or_default().push(i);
    }
    let mut used = vec![false; new.chunks.len()];
    let mut blocks = Vec::new();
    for old_chunk in &old.chunks {
        let key = chunk_key(old_chunk);
        let mut best: Option<(usize, f32)> = None;
        if let Some(candidates) = index.get(&key) {
            for &i in candidates {
                if used[i] {
                    continue;
                }
                let sim = jaccard(&old_chunk.markdown, &new.chunks[i].markdown);
                if sim >= MODIFIED_MIN_SIM && best.is_none_or(|(_, b)| sim > b) {
                    best = Some((i, sim));
                }
            }
        }
        match best {
            Some((i, sim)) => {
                used[i] = true;
                let new_chunk = &new.chunks[i];
                let identical = normalize(&old_chunk.markdown) == normalize(&new_chunk.markdown);
                let (status, similarity) = if identical || sim >= UNCHANGED_SIM {
                    (DiffStatus::Unchanged, 1.0)
                } else {
                    (DiffStatus::Modified, sim)
                };
                blocks.push(BlockDiff {
                    status,
                    section_path: key.0.clone(),
                    block_kind: key.1.clone(),
                    old_text: Some(old_chunk.markdown.clone()),
                    new_text: Some(new_chunk.markdown.clone()),
                    old_page: Some(old_chunk.page_index as u32),
                    new_page: Some(new_chunk.page_index as u32),
                    old_bbox: Some(old_chunk.bbox),
                    new_bbox: Some(new_chunk.bbox),
                    similarity,
                    delta_summary: if status == DiffStatus::Modified {
                        delta_summary(&old_chunk.markdown, &new_chunk.markdown)
                    } else {
                        String::new()
                    },
                });
            }
            None => blocks.push(BlockDiff {
                status: DiffStatus::Removed,
                section_path: key.0.clone(),
                block_kind: key.1.clone(),
                old_text: Some(old_chunk.markdown.clone()),
                new_text: None,
                old_page: Some(old_chunk.page_index as u32),
                new_page: None,
                old_bbox: Some(old_chunk.bbox),
                new_bbox: None,
                similarity: 0.0,
                delta_summary: format!("{} words removed", word_count(&old_chunk.markdown)),
            }),
        }
    }
    for (i, chunk) in new.chunks.iter().enumerate() {
        if used[i] {
            continue;
        }
        let key = chunk_key(chunk);
        blocks.push(BlockDiff {
            status: DiffStatus::Added,
            section_path: key.0.clone(),
            block_kind: key.1.clone(),
            old_text: None,
            new_text: Some(chunk.markdown.clone()),
            old_page: None,
            new_page: Some(chunk.page_index as u32),
            old_bbox: None,
            new_bbox: Some(chunk.bbox),
            similarity: 0.0,
            delta_summary: format!("{} words added", word_count(&chunk.markdown)),
        });
    }
    let mut diff = DocumentDiff {
        old_path: old.source_path.clone(),
        new_path: new.source_path.clone(),
        blocks,
        unchanged: 0,
        modified: 0,
        added: 0,
        removed: 0,
    };
    for block in &diff.blocks {
        match block.status {
            DiffStatus::Unchanged => diff.unchanged += 1,
            DiffStatus::Modified => diff.modified += 1,
            DiffStatus::Added => diff.added += 1,
            DiffStatus::Removed => diff.removed += 1,
        }
    }
    diff
}

/// Render a human-readable Markdown redline summary (skips Unchanged).
pub fn to_markdown_diff(diff: &DocumentDiff) -> String {
    let mut out = format!(
        "# Document diff: {} → {}\n\nUnchanged: {} · Modified: {} · Added: {} · Removed: {}\n",
        diff.old_path, diff.new_path, diff.unchanged, diff.modified, diff.added, diff.removed
    );
    for block in &diff.blocks {
        if block.status == DiffStatus::Unchanged {
            continue;
        }
        out.push_str(&format!(
            "\n## {:?} — {} ({})\n",
            block.status,
            if block.section_path.is_empty() {
                "(untitled)"
            } else {
                &block.section_path
            },
            page_range(block)
        ));
        if !block.delta_summary.is_empty() {
            out.push_str(&format!("*{}*\n", block.delta_summary));
        }
        if let Some(old) = &block.old_text {
            out.push_str(&format!("- {}\n", truncate(old, REDLINE_TEXT_LIMIT)));
        }
        if let Some(new) = &block.new_text {
            out.push_str(&format!("+ {}\n", truncate(new, REDLINE_TEXT_LIMIT)));
        }
    }
    out
}

/// Alignment key: breadcrumb trail plus real block kind.
fn chunk_key(chunk: &GroundedChunk) -> (String, String) {
    (chunk.breadcrumb.join(" > "), chunk.block_kind.clone())
}

/// Collapse whitespace runs for stable comparison.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercased whitespace-token Jaccard similarity.
fn jaccard(a: &str, b: &str) -> f32 {
    let a_set: HashSet<String> = normalize(a)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let b_set: HashSet<String> = normalize(b)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if a_set.is_empty() && b_set.is_empty() {
        return 1.0;
    }
    let inter = a_set.intersection(&b_set).count();
    let union = a_set.union(&b_set).count().max(1);
    inter as f32 / union as f32
}

fn word_count(text: &str) -> usize {
    normalize(text).split_whitespace().count()
}

/// Word-level (or table-row-level) delta between two revisions.
fn delta_summary(old: &str, new: &str) -> String {
    if is_table(old) && is_table(new) {
        let old_rows = table_rows(old);
        let new_rows = table_rows(new);
        let changed = old_rows
            .iter()
            .zip(new_rows.iter())
            .filter(|(a, b)| a != b)
            .count()
            + old_rows.len().abs_diff(new_rows.len());
        return format!(
            "{changed}/{} table rows changed",
            old_rows.len().max(new_rows.len())
        );
    }
    let old_set: HashSet<String> = normalize(old)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let new_set: HashSet<String> = normalize(new)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let removed = old_set.difference(&new_set).count();
    let added = new_set.difference(&old_set).count();
    format!("-{removed} +{added} words")
}

/// True for Markdown table grids (header + separator rows).
fn is_table(text: &str) -> bool {
    let mut lines = text.lines().map(str::trim);
    matches!((lines.next(), lines.next()), (Some(h), Some(s)) if h.starts_with('|') && s.starts_with('|'))
}

/// Body rows of a Markdown table (header and separator dropped).
fn table_rows(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|'))
        .skip(2)
        .map(str::to_string)
        .collect()
}

/// `p1 → p2` page range for a block diff (1-based display).
fn page_range(block: &BlockDiff) -> String {
    match (block.old_page, block.new_page) {
        (Some(o), Some(n)) if o == n => format!("p{}", o + 1),
        (Some(o), Some(n)) => format!("p{} → p{}", o + 1, n + 1),
        (Some(o), None) => format!("p{}", o + 1),
        (None, Some(n)) => format!("p{}", n + 1),
        (None, None) => "p?".to_string(),
    }
}

/// Truncate to `limit` chars (single-line, with ellipsis).
fn truncate(text: &str, limit: usize) -> String {
    let flat = normalize(text);
    if flat.chars().count() <= limit {
        return flat;
    }
    format!("{}…", flat.chars().take(limit).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BBox, GroundedChunk, PageBBox};

    fn chunk(id: &str, section: &str, markdown: &str) -> GroundedChunk {
        let bbox = BBox::new(72.0, 100.0, 540.0, 120.0);
        GroundedChunk {
            chunk_id: id.to_string(),
            page_index: 0,
            bbox,
            breadcrumb: vec![section.to_string()],
            markdown: markdown.to_string(),
            token_estimate: 10,
            parsed_by_vlm: false,
            page_spans: vec![PageBBox { page: 0, bbox }],
            footnotes: Vec::new(),
            block_kind: "paragraph".to_string(),
            triage_score: 0.0,
        }
    }

    fn doc(path: &str, chunks: Vec<GroundedChunk>) -> ParsedDocument {
        ParsedDocument {
            source_path: path.to_string(),
            total_pages: 1,
            fast_path_pages: 1,
            vlm_fallback_regions: 0,
            elapsed_ms: 1,
            markdown: String::new(),
            chunks,
            form_fields: Vec::new(),
            footnotes: Vec::new(),
            text_lines: Vec::new(),
        }
    }

    #[test]
    fn classifies_all_four_statuses() {
        let old = doc(
            "old.pdf",
            vec![
                chunk("a", "S1", "The quick brown fox jumps."),
                chunk("b", "S2", "Revenue grew twenty percent yearly."),
                chunk("c", "S3", "This paragraph will vanish entirely now."),
            ],
        );
        let new = doc(
            "new.pdf",
            vec![
                chunk("a", "S1", "The quick brown fox jumps."),
                chunk("b", "S2", "Revenue grew thirty percent yearly overall."),
                chunk("d", "S4", "A brand new paragraph appears here today."),
            ],
        );
        let diff = diff_documents(&old, &new);
        assert_eq!(diff.old_path, "old.pdf");
        assert_eq!(diff.new_path, "new.pdf");
        let status = |section: &str| {
            diff.blocks
                .iter()
                .find(|b| b.section_path == section)
                .map(|b| b.status)
        };
        assert_eq!(status("S1"), Some(DiffStatus::Unchanged));
        assert_eq!(status("S2"), Some(DiffStatus::Modified));
        assert_eq!(status("S3"), Some(DiffStatus::Removed));
        assert_eq!(status("S4"), Some(DiffStatus::Added));
        assert_eq!(
            (diff.unchanged, diff.modified, diff.added, diff.removed),
            (1, 1, 1, 1)
        );
        let changed = diff
            .blocks
            .iter()
            .find(|b| b.section_path == "S2")
            .expect("S2");
        assert!(changed.old_bbox.is_some() && changed.new_bbox.is_some());
        assert!(!changed.delta_summary.is_empty());
    }

    #[test]
    fn markdown_redline_skips_unchanged() {
        let old = doc("o.pdf", vec![chunk("a", "S1", "Same text here.")]);
        let new = doc(
            "n.pdf",
            vec![
                chunk("a", "S1", "Same text here."),
                chunk("b", "S2", "Fresh extra paragraph content."),
            ],
        );
        let md = to_markdown_diff(&diff_documents(&old, &new));
        assert!(md.contains("Added"), "md:\n{md}");
        assert!(md.contains("Fresh extra paragraph"), "md:\n{md}");
        assert!(!md.contains("Same text here"), "md:\n{md}");
    }

    #[test]
    fn table_cells_summarize_by_row() {
        let old_table = "| Item | Qty |\n| --- | --- |\n| Apples | 10 |\n| Pears | 4 |";
        let new_table = "| Item | Qty |\n| --- | --- |\n| Apples | 12 |\n| Pears | 4 |";
        let summary = delta_summary(old_table, new_table);
        assert_eq!(summary, "1/2 table rows changed");
    }
}
