//! Pure-Rust BM25 retrieval over grounded chunks.
//!
//! Shared by the MCP `search_pdf_chunks` tool and the Python
//! `search_chunks` binding: term ranking plus an exact-phrase boost,
//! no model calls.

use std::collections::{HashMap, HashSet};

use crate::models::GroundedChunk;

/// Lowercase alphanumeric tokens.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// BM25-ranked `(chunk index, score)` pairs, highest first.
///
/// Exact-phrase matches get a multiplicative + additive boost so a
/// partial-word query (`tabl`) still retrieves `tabled …` chunks that
/// share no whole token with the query.
#[must_use]
pub fn bm25_rank(chunks: &[GroundedChunk], query: &str, top_k: usize) -> Vec<(usize, f32)> {
    const K1: f32 = 1.2;
    const B: f32 = 0.75;
    let docs: Vec<Vec<String>> = chunks.iter().map(|c| tokenize(&c.markdown)).collect();
    let query_terms = tokenize(query);
    if query_terms.is_empty() || docs.is_empty() {
        return Vec::new();
    }
    let n = docs.len() as f32;
    let avgdl = docs.iter().map(Vec::len).sum::<usize>() as f32 / n;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for terms in &docs {
        let mut seen = HashSet::new();
        for term in terms {
            if seen.insert(term.as_str()) {
                *df.entry(term.as_str()).or_insert(0) += 1;
            }
        }
    }
    let qlower = query.to_lowercase();
    let mut scored: Vec<(usize, f32)> = docs
        .iter()
        .enumerate()
        .map(|(i, terms)| {
            let mut score = 0.0;
            for qt in &query_terms {
                let tf = terms.iter().filter(|t| *t == qt).count() as f32;
                if tf == 0.0 {
                    continue;
                }
                let df_t = df.get(qt.as_str()).copied().unwrap_or(1).max(1) as f32;
                let idf = (1.0 + (n - df_t + 0.5) / (df_t + 0.5)).ln();
                score += idf * tf * (K1 + 1.0)
                    / (tf + K1 * (1.0 - B + B * terms.len() as f32 / avgdl.max(1.0)));
            }
            if chunks[i].markdown.to_lowercase().contains(&qlower) {
                score = score.mul_add(1.5, 1.0);
            }
            (i, score)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k.max(1));
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BBox, GroundedChunk, PageBBox};

    fn chunk(id: &str, markdown: &str) -> GroundedChunk {
        let bbox = BBox::new(0.0, 0.0, 10.0, 10.0);
        GroundedChunk {
            chunk_id: id.to_string(),
            page_index: 0,
            bbox,
            breadcrumb: Vec::new(),
            markdown: markdown.to_string(),
            token_estimate: 0,
            parsed_by_vlm: false,
            page_spans: vec![PageBBox { page: 0, bbox }],
            footnotes: Vec::new(),
            block_kind: "paragraph".to_string(),
            triage_score: 0.0,
        }
    }

    #[test]
    fn ranks_term_matches_first() {
        let chunks = vec![
            chunk("a", "invoice invoice totals"),
            chunk("b", "invoice summary"),
            chunk("c", "unrelated prose here"),
        ];
        let ranked = bm25_rank(&chunks, "invoice", 5);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, 0);
        assert_eq!(ranked[1].0, 1);
        assert!(ranked[0].1 > ranked[1].1);
    }

    #[test]
    fn empty_query_returns_empty() {
        let chunks = vec![chunk("a", "some text")];
        assert!(bm25_rank(&chunks, "", 5).is_empty());
        assert!(bm25_rank(&chunks, "   ", 5).is_empty());
    }

    #[test]
    fn substring_boost_matches_partial_words() {
        let chunks = vec![
            chunk("a", "tabled data here"),
            chunk("b", "unrelated words only"),
        ];
        // "tabl" is no whole token anywhere, but chunk (a) contains it.
        let ranked = bm25_rank(&chunks, "tabl", 5);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, 0);
    }

    #[test]
    fn top_k_truncates() {
        let chunks = vec![
            chunk("a", "common one"),
            chunk("b", "common two"),
            chunk("c", "common three"),
        ];
        assert_eq!(bm25_rank(&chunks, "common", 2).len(), 2);
    }
}
