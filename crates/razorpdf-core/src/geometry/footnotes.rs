//! Running header/footer stripping and footnote graph linking.
//!
//! Repeating margin bands (normalized, digit-blind) are stripped from
//! body flow; bottom-margin footnote definitions become [`FootnoteItem`]s
//! whose in-text markers rewrite to `[^n]` Markdown footnotes.

use std::collections::{BTreeMap, HashSet};

use crate::models::{BlockKind, FootnoteItem, LayoutBlock};

/// Top/bottom margin fraction scanned for running bands.
const BAND_FRAC: f32 = 0.08;
/// Same-band vertical tolerance (±pt) for repeat grouping.
const BAND_TOL_PT: f32 = 6.0;
/// Bottom fraction of the page scanned for footnote definitions.
const FOOTNOTE_ZONE_FRAC: f32 = 0.20;

/// Outcome counters for one [`process_pages`] pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FootnoteStats {
    /// Running header/footer blocks removed.
    pub stripped_running_blocks: usize,
    /// Footnote definitions extracted.
    pub footnotes_found: usize,
    /// In-text markers rewritten to `[^marker]`.
    pub markers_linked: usize,
}

/// Strip running headers/footers and extract footnote definitions.
///
/// `sizes[i]` is the `(width, height)` of page `i` in points. Running
/// detection needs ≥ 3 pages; footnote extraction runs on any page
/// count. Removed footnote definitions return as [`FootnoteItem`]s in
/// page order; body markers on the same page rewrite to `[^marker]`.
pub fn process_pages(
    pages: &mut [Vec<LayoutBlock>],
    sizes: &[(f32, f32)],
) -> (Vec<FootnoteItem>, FootnoteStats) {
    let mut stats = FootnoteStats::default();
    if pages.len() >= 3 {
        stats.stripped_running_blocks = strip_running_bands(pages, sizes);
    }
    let mut footnotes = Vec::new();
    for (i, page) in pages.iter_mut().enumerate() {
        let height = sizes.get(i).map(|s| s.1).unwrap_or(792.0);
        extract_page_footnotes(page, height, &mut footnotes, &mut stats);
    }
    for page in pages.iter_mut() {
        link_page_markers(page, &footnotes, &mut stats);
    }
    (footnotes, stats)
}

// ---------------------------------------------------------------------------
// Running header/footer detection
// ---------------------------------------------------------------------------

/// Margin band of a block: header (top 8%), footer (bottom 8%), or body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Band {
    Header,
    Footer,
}

fn block_band(bbox_y0: f32, bbox_y1: f32, height: f32) -> Option<(Band, f32)> {
    if bbox_y0 <= BAND_FRAC * height {
        Some((Band::Header, bbox_y0))
    } else if bbox_y1 >= (1.0 - BAND_FRAC) * height {
        Some((Band::Footer, bbox_y1))
    } else {
        None
    }
}

/// Normalize a margin line for repeat grouping: lowercase, collapsed
/// whitespace, every digit run replaced by `#` (page numbers vanish).
fn normalize_margin_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_digit = false;
    let mut in_space = true; // trim leading
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !in_digit {
                out.push('#');
                in_digit = true;
            }
            in_space = false;
        } else if ch.is_whitespace() {
            in_digit = false;
            in_space = true;
        } else {
            if in_space && !out.is_empty() {
                out.push(' ');
            }
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            in_digit = false;
            in_space = false;
        }
    }
    out
}

/// One margin-band hit: (page index, band-edge y, block index).
type BandHit = (usize, f32, usize);

/// Remove blocks whose normalized margin text repeats across pages at
/// the same vertical band. Returns the removal count.
fn strip_running_bands(pages: &mut [Vec<LayoutBlock>], sizes: &[(f32, f32)]) -> usize {
    // (band, normalized) → hits.
    let mut groups: BTreeMap<(Band, String), Vec<BandHit>> = BTreeMap::new();
    for (i, page) in pages.iter().enumerate() {
        let height = sizes.get(i).map(|s| s.1).unwrap_or(792.0);
        for (j, block) in page.iter().enumerate() {
            if !matches!(block.kind, BlockKind::Paragraph | BlockKind::Heading { .. }) {
                continue;
            }
            let text = block.raw_text.trim();
            if text.is_empty() || text.contains('\n') || text.len() > 200 {
                continue;
            }
            let Some((band, y)) = block_band(block.bbox.y0, block.bbox.y1, height) else {
                continue;
            };
            groups
                .entry((band, normalize_margin_line(text)))
                .or_default()
                .push((i, y, j));
        }
    }
    // A group strips when one ±6pt run covers ≥50% of pages or ≥3 pages.
    let threshold = pages.len().div_ceil(2); // ceil(n/2)
    let mut strip: Vec<(usize, usize)> = Vec::new();
    for hits in groups.into_values() {
        let mut sorted = hits;
        sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut best: Vec<(usize, f32, usize)> = Vec::new();
        let mut run: Vec<(usize, f32, usize)> = Vec::new();
        for hit in sorted {
            if run.is_empty() || hit.1 - run[0].1 <= 2.0 * BAND_TOL_PT {
                run.push(hit);
            } else {
                if run.len() > best.len() {
                    best = std::mem::take(&mut run);
                }
                run = vec![hit];
            }
        }
        if run.len() > best.len() {
            best = run;
        }
        let distinct: HashSet<usize> = best.iter().map(|h| h.0).collect();
        if distinct.len() >= threshold || distinct.len() >= 3 {
            strip.extend(best.into_iter().map(|(page, _, idx)| (page, idx)));
        }
    }
    strip.sort_unstable();
    strip.dedup();
    // Remove back-to-front per page so indices stay valid.
    strip.sort_by(|a, b| b.cmp(a));
    for (page, idx) in &strip {
        if let Some(blocks) = pages.get_mut(*page) {
            if *idx < blocks.len() {
                blocks.remove(*idx);
            }
        }
    }
    strip.len()
}

// ---------------------------------------------------------------------------
// Footnote definitions
// ---------------------------------------------------------------------------

/// Parse a footnote definition line into (canonical marker, body).
/// Accepts `1 text`, `1. text`, `12) text`, `[1] text`, `* text`, `† text`.
fn parse_footnote_def(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    // Symbol markers first (single non-alphanumeric char + body).
    if let Some(first) = trimmed.chars().next() {
        if matches!(first, '*' | '†' | '‡' | '§' | '¶') {
            let body: String = trimmed[first.len_utf8()..].trim().to_string();
            if !body.is_empty() {
                return Some((first.to_string(), body));
            }
            return None;
        }
    }
    // Bracket form: [12] body.
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some((digits, tail)) = rest.split_once(']') {
            if !digits.is_empty() && digits.len() <= 3 && digits.chars().all(|c| c.is_ascii_digit())
            {
                let body = tail.trim().to_string();
                if !body.is_empty() {
                    return Some((digits.to_string(), body));
                }
            }
            return None;
        }
    }
    // Bare digits + separator: 1 body / 1. body / 1) body.
    let digit_len: usize = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .map(char::len_utf8)
        .sum();
    if digit_len == 0 || digit_len > 3 {
        return None;
    }
    let (digits, tail) = trimmed.split_at(digit_len);
    let mut tail_chars = tail.chars();
    match tail_chars.next() {
        Some(' ') | Some('\t') | Some('.') | Some(')') => {}
        _ => return None,
    }
    let body = tail
        .trim_start_matches([' ', '\t', '.', ')'])
        .trim()
        .to_string();
    if body.is_empty() {
        return None;
    }
    Some((digits.to_string(), body))
}

/// Lift bottom-zone footnote definitions out of a page's blocks.
fn extract_page_footnotes(
    page: &mut Vec<LayoutBlock>,
    height: f32,
    footnotes: &mut Vec<FootnoteItem>,
    stats: &mut FootnoteStats,
) {
    let zone_top = (1.0 - FOOTNOTE_ZONE_FRAC) * height;
    let mut defs: Vec<usize> = Vec::new();
    for (j, block) in page.iter().enumerate() {
        if !matches!(block.kind, BlockKind::Paragraph) || block.bbox.y0 < zone_top {
            continue;
        }
        if parse_footnote_def(&block.raw_text).is_some() {
            defs.push(j);
        }
    }
    for j in defs.into_iter().rev() {
        let block = page.remove(j);
        if let Some((marker, text)) = parse_footnote_def(&block.raw_text) {
            footnotes.push(FootnoteItem {
                marker,
                text,
                page: block.page_index as u32,
                bbox: block.bbox,
            });
            stats.footnotes_found += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Marker linking
// ---------------------------------------------------------------------------

/// Rewrite in-text markers (`word1`, `claim,2`) to `[^1]` for footnotes
/// defined on the same page. Only markers with a same-page definition
/// link, so body numbers without definitions never rewrite.
fn link_page_markers(
    page: &mut [LayoutBlock],
    footnotes: &[FootnoteItem],
    stats: &mut FootnoteStats,
) {
    for block in page.iter_mut() {
        if !matches!(block.kind, BlockKind::Paragraph | BlockKind::Heading { .. }) {
            continue;
        }
        let defined: Vec<&str> = footnotes
            .iter()
            .filter(|f| f.page == block.page_index as u32)
            .map(|f| f.marker.as_str())
            .collect();
        if defined.is_empty() {
            continue;
        }
        let (rewritten, count) = rewrite_markers(&block.raw_text, &defined);
        if count > 0 {
            block.raw_text = rewritten;
            stats.markers_linked += count;
        }
    }
}

/// Single-pass marker rewrite over one text. Returns (text, replacements).
///
/// ASCII digits link only after punctuation/closers or full words
/// (≥2 letters), so algebraic variables (`n2`, `x1`) never rewrite;
/// bracketed `[n]` and Unicode superscripts (`¹²³`) always link as
/// explicit marker glyphs.
fn rewrite_markers(text: &str, markers: &[&str]) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + markers.len() * 4);
    let mut count = 0;
    let mut i = 0;
    while i < chars.len() {
        // Symbol markers: alnum + symbol, not doubled (avoids `**bold**`).
        if matches!(chars[i], '*' | '†' | '‡' | '§' | '¶')
            && markers.contains(&chars[i].to_string().as_str())
            && i > 0
            && (chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '.' | ',' | ';' | ':'))
            && chars.get(i + 1).is_none_or(|c| *c != chars[i])
            && (i < 2 || chars[i - 2] != chars[i])
        {
            out.push_str(&format!("[^{}]", chars[i]));
            count += 1;
            i += 1;
            continue;
        }
        // Explicit bracketed markers: `claim [1]` → `claim [^1]`.
        if chars[i] == '[' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && chars.get(j) == Some(&']') {
                let run: String = chars[i + 1..j].iter().collect();
                if markers.contains(&run.as_str()) {
                    out.push_str(&format!("[^{run}]"));
                    count += 1;
                    i = j + 1;
                    continue;
                }
            }
        }
        // Unicode superscripts are unambiguous marker glyphs: link freely.
        if let Some(digit) = superscript_digit(chars[i]) {
            let marker = digit.to_string();
            if markers.contains(&marker.as_str()) {
                out.push_str(&format!("[^{marker}]"));
                count += 1;
                i += 1;
                continue;
            }
        }
        // Numeric runs exactly equal to a defined marker, glued to
        // punctuation/closers or a full word (single-letter variables
        // like `n2` are math, never footnotes).
        if chars[i].is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let run: String = chars[i..j].iter().collect();
            let prev_ok = match chars.get(i.wrapping_sub(1)) {
                _ if i == 0 => false,
                Some(c) if c.is_alphanumeric() => i >= 2 && chars[i - 2].is_alphanumeric(),
                Some(c) => {
                    matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '"' | '”' | ')')
                }
                None => false,
            };
            let next_ok = chars
                .get(j)
                .is_none_or(|c| !c.is_alphanumeric() && *c != '[');
            if prev_ok && next_ok && markers.contains(&run.as_str()) {
                out.push_str(&format!("[^{run}]"));
                count += 1;
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, count)
}

/// Map Unicode superscript digits to ASCII (`¹` → `1`).
fn superscript_digit(ch: char) -> Option<char> {
    Some(match ch {
        '\u{2070}' => '0',
        '\u{00B9}' => '1',
        '\u{00B2}' => '2',
        '\u{00B3}' => '3',
        '\u{2074}' => '4',
        '\u{2075}' => '5',
        '\u{2076}' => '6',
        '\u{2077}' => '7',
        '\u{2078}' => '8',
        '\u{2079}' => '9',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BBox, BlockKind, PageBBox};

    fn para(page: usize, order: usize, text: &str) -> LayoutBlock {
        let bbox = BBox::new(72.0, 300.0, 540.0, 312.0);
        LayoutBlock {
            id: format!("p{page}-line{order}"),
            page_index: page,
            bbox,
            kind: BlockKind::Paragraph,
            raw_text: text.to_string(),
            reading_order: order,
            page_spans: vec![PageBBox {
                page: page as u32,
                bbox,
            }],
        }
    }

    fn sizes(n: usize) -> Vec<(f32, f32)> {
        vec![(612.0, 792.0); n]
    }

    #[test]
    fn strips_repeating_footer_preserves_footnote() {
        let mut pages = vec![
            vec![
                para(0, 0, "Body text with a claim1 about parsing."),
                para(0, 1, "1 See Section 4.2 for details."),
                para(0, 2, "Confidential - Page 1"),
            ],
            vec![
                para(1, 0, "More body text here."),
                para(1, 1, "Confidential - Page 2"),
            ],
            vec![
                para(2, 0, "Final body text."),
                para(2, 1, "Confidential - Page 3"),
            ],
        ];
        // Place body at mid-page, footnote above the footer band, footer
        // inside the bottom 8% (y > 728.6 on 792pt pages).
        pages[0][0].bbox = BBox::new(72.0, 300.0, 540.0, 312.0);
        pages[0][1].bbox = BBox::new(72.0, 660.0, 540.0, 672.0);
        pages[0][2].bbox = BBox::new(72.0, 760.0, 540.0, 772.0);
        pages[1][0].bbox = BBox::new(72.0, 300.0, 540.0, 312.0);
        pages[1][1].bbox = BBox::new(72.0, 760.0, 540.0, 772.0);
        pages[2][0].bbox = BBox::new(72.0, 300.0, 540.0, 312.0);
        pages[2][1].bbox = BBox::new(72.0, 760.0, 540.0, 772.0);

        let (footnotes, stats) = process_pages(&mut pages, &sizes(3));
        assert_eq!(stats.stripped_running_blocks, 3);
        assert_eq!(stats.footnotes_found, 1);
        assert_eq!(footnotes.len(), 1);
        assert_eq!(footnotes[0].marker, "1");
        assert_eq!(footnotes[0].text, "See Section 4.2 for details.");
        assert_eq!(footnotes[0].page, 0);
        // Footer gone, footnote def lifted out of flow, marker linked.
        assert_eq!(pages[0].len(), 1);
        assert_eq!(pages[1].len(), 1);
        assert_eq!(pages[2].len(), 1);
        assert!(
            pages[0][0].raw_text.contains("[^1]"),
            "body: {}",
            pages[0][0].raw_text
        );
        assert!(stats.markers_linked >= 1);
    }

    #[test]
    fn unique_footers_survive() {
        let mut pages = vec![
            vec![para(0, 0, "Body one.")],
            vec![para(1, 0, "Body two.")],
            vec![para(2, 0, "Body three.")],
        ];
        let closings = [
            "Alpha chapter ending",
            "Beta chapter ending",
            "Gamma chapter ending",
        ];
        for (i, page) in pages.iter_mut().enumerate() {
            page[0].bbox = BBox::new(72.0, 300.0, 540.0, 312.0);
            page.push(para(i, 1, closings[i]));
            page[1].bbox = BBox::new(72.0, 760.0, 540.0, 772.0);
        }
        let (footnotes, stats) = process_pages(&mut pages, &sizes(3));
        assert_eq!(stats.stripped_running_blocks, 0);
        assert!(footnotes.is_empty());
        assert!(pages.iter().all(|p| p.len() == 2));
    }

    #[test]
    fn short_documents_skip_running_detection() {
        let mut pages = vec![
            vec![para(0, 0, "Same footer")],
            vec![para(1, 0, "Same footer")],
        ];
        for page in pages.iter_mut() {
            page[0].bbox = BBox::new(72.0, 760.0, 540.0, 772.0);
        }
        let (_, stats) = process_pages(&mut pages, &sizes(2));
        assert_eq!(stats.stripped_running_blocks, 0);
    }

    #[test]
    fn bracket_and_symbol_markers_parse() {
        assert_eq!(
            parse_footnote_def("[12] Bracketed note."),
            Some(("12".to_string(), "Bracketed note.".to_string()))
        );
        assert_eq!(
            parse_footnote_def("† Dagger note."),
            Some(("†".to_string(), "Dagger note.".to_string()))
        );
        assert_eq!(parse_footnote_def("2024 was a busy year."), None);
        assert_eq!(parse_footnote_def("No marker here."), None);
        assert_eq!(parse_footnote_def("1"), None);
    }

    #[test]
    fn unlinked_numbers_never_rewrite() {
        // Marker "2" has no definition: "Section 2" and "v2" survive.
        let (text, count) = rewrite_markers("See Section 2 and v2 now.", &["1"]);
        assert_eq!(count, 0);
        assert_eq!(text, "See Section 2 and v2 now.");
        // Spaced numbers never rewrite even when defined.
        let (text, count) = rewrite_markers("See Section 1 now.", &["1"]);
        assert_eq!(count, 0);
        assert_eq!(text, "See Section 1 now.");
    }

    #[test]
    fn bold_markdown_stars_survive() {
        let (text, count) = rewrite_markers("A **bold** claim* here.", &["*"]);
        assert_eq!(count, 1);
        assert_eq!(text, "A **bold** claim[^*] here.");
    }

    #[test]
    fn algebraic_variables_never_link() {
        let (text, count) = rewrite_markers("For n2 and x1 terms with a2 values.", &["1", "2"]);
        assert_eq!(count, 0);
        assert_eq!(text, "For n2 and x1 terms with a2 values.");
    }

    #[test]
    fn full_words_still_link() {
        let (text, count) = rewrite_markers("A strong claim1 holds.", &["1"]);
        assert_eq!(count, 1);
        assert_eq!(text, "A strong claim[^1] holds.");
    }

    #[test]
    fn punctuation_adjacent_markers_link() {
        let (text, count) = rewrite_markers("A claim.2 Needs proof.", &["2"]);
        assert_eq!(count, 1);
        assert_eq!(text, "A claim.[^2] Needs proof.");
    }

    #[test]
    fn bracketed_markers_link() {
        let (text, count) = rewrite_markers("A claim [1] with evidence.", &["1"]);
        assert_eq!(count, 1);
        assert_eq!(text, "A claim [^1] with evidence.");
    }

    #[test]
    fn unicode_superscripts_link() {
        let (text, count) = rewrite_markers("A claim¹ with evidence.", &["1"]);
        assert_eq!(count, 1);
        assert_eq!(text, "A claim[^1] with evidence.");
        let (text, count) = rewrite_markers("x² plus y³ terms.", &["2", "3"]);
        assert_eq!(count, 2);
        assert_eq!(text, "x[^2] plus y[^3] terms.");
    }
}
