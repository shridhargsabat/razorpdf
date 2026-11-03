//! Columnar and JSONL export for data/vector pipelines.
//!
//! [`chunks_to_columnar_batch`] packs chunks Structure-of-Arrays style so
//! Python loads them loop-free (`polars.from_dict`,
//! `pyarrow.Table.from_pydict`); [`chunks_to_jsonl`] streams one JSON
//! record per line.

use anyhow::{Context, Result};

use crate::models::{ColumnarChunkBatch, GroundedChunk};

/// Pack chunks into a columnar batch (all columns share one length).
pub fn chunks_to_columnar_batch(chunks: &[GroundedChunk]) -> ColumnarChunkBatch {
    ColumnarChunkBatch {
        chunk_id: chunks.iter().map(|c| c.chunk_id.clone()).collect(),
        page: chunks.iter().map(|c| c.page_index as u32).collect(),
        bbox_x0: chunks.iter().map(|c| c.bbox.x0).collect(),
        bbox_y0: chunks.iter().map(|c| c.bbox.y0).collect(),
        bbox_x1: chunks.iter().map(|c| c.bbox.x1).collect(),
        bbox_y1: chunks.iter().map(|c| c.bbox.y1).collect(),
        token_count: chunks.iter().map(|c| c.token_estimate).collect(),
        triage_score: chunks.iter().map(|c| c.triage_score).collect(),
        block_kind: chunks.iter().map(|c| c.block_kind.clone()).collect(),
        section_path: chunks.iter().map(|c| c.breadcrumb.join(" > ")).collect(),
        text: chunks.iter().map(|c| c.markdown.clone()).collect(),
    }
}

/// Render chunks as newline-delimited JSON (one record per line).
pub fn chunks_to_jsonl(chunks: &[GroundedChunk]) -> Result<String> {
    let mut out = String::new();
    for chunk in chunks {
        out.push_str(&serde_json::to_string(chunk).context("encode chunk")?);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BBox, PageBBox};

    fn chunk(id: &str, page: usize, markdown: &str) -> GroundedChunk {
        let bbox = BBox::new(72.0, 100.0, 540.0, 120.0);
        GroundedChunk {
            chunk_id: id.to_string(),
            page_index: page,
            bbox,
            breadcrumb: vec!["Report".to_string(), "Intro".to_string()],
            markdown: markdown.to_string(),
            token_estimate: 12,
            parsed_by_vlm: false,
            page_spans: vec![PageBBox {
                page: page as u32,
                bbox,
            }],
            footnotes: Vec::new(),
            block_kind: "custom-kind".to_string(),
            triage_score: 0.42,
        }
    }

    #[test]
    fn batch_columns_align_with_chunks() {
        let chunks = vec![
            chunk("p0-000", 0, "Hello world."),
            chunk("p1-000", 1, "| A |\n| --- |\n| b |"),
        ];
        let batch = chunks_to_columnar_batch(&chunks);
        assert_eq!(batch.chunk_id, vec!["p0-000", "p1-000"]);
        assert_eq!(batch.page, vec![0, 1]);
        assert_eq!(batch.bbox_x0.len(), 2);
        assert_eq!(batch.token_count, vec![12, 12]);
        assert_eq!(batch.block_kind, vec!["custom-kind", "custom-kind"]);
        assert_eq!(batch.section_path, vec!["Report > Intro", "Report > Intro"]);
        assert_eq!(batch.text[1], "| A |\n| --- |\n| b |");
        assert_eq!(batch.triage_score, vec![0.42, 0.42]);
    }

    #[test]
    fn jsonl_emits_one_record_per_line() {
        let chunks = vec![chunk("p0-000", 0, "Hello world.")];
        let jsonl = chunks_to_jsonl(&chunks).expect("jsonl");
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 1);
        let record: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
        assert_eq!(record["chunk_id"], serde_json::json!("p0-000"));
        assert_eq!(record["markdown"], serde_json::json!("Hello world."));
    }
}
