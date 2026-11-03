//! Deterministic spatial JSON Schema extractor.
//!
//! Four-tier matching per property: AcroForm fields → inline
//! `Label: value` lines → spatial right-of/below chunk adjacency → table
//! header-to-row mapping for arrays of objects. Every hit carries page,
//! bbox, confidence, and source provenance.

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::models::{BBox, ParsedDocument, SchemaExtractionResult, SchemaFieldMatch};

/// Confidence per match tier.
const CONF_ACROFORM: f32 = 0.98;
const CONF_INLINE_KV: f32 = 0.85;
const CONF_SPATIAL_KV: f32 = 0.70;
const CONF_TABLE_COLUMN: f32 = 0.90;

/// Extract structured data matching `schema` (JSON Schema object with
/// `properties`) from an assembled document.
///
/// Unmatched properties are omitted from `data` (callers escalate
/// required fields with confidence < 0.70 to the VLM).
pub fn extract_with_schema(doc: &ParsedDocument, schema: &Value) -> Result<SchemaExtractionResult> {
    let obj = schema.as_object().context("schema must be a JSON object")?;
    let props = obj
        .get("properties")
        .and_then(Value::as_object)
        .context("schema object needs a \"properties\" map")?;
    let mut data = Map::new();
    let mut provenance = Vec::new();
    for (name, prop) in props {
        if is_array_of_objects(prop) {
            if let Some(hit) = match_table_array(doc, name, prop) {
                data.insert(name.clone(), hit.value.clone());
                provenance.push(hit);
            }
            continue;
        }
        let target = prop.get("type").and_then(Value::as_str).unwrap_or("string");
        if let Some(hit) = match_acroform(doc, name, prop, target)
            .or_else(|| match_inline_kv(doc, name, prop, target))
            .or_else(|| match_spatial_kv(doc, name, prop, target))
        {
            data.insert(name.clone(), hit.value.clone());
            provenance.push(hit);
        }
    }
    let overall_confidence = if provenance.is_empty() {
        0.0
    } else {
        provenance.iter().map(|m| m.confidence).sum::<f32>() / provenance.len() as f32
    };
    Ok(SchemaExtractionResult {
        data: Value::Object(data),
        provenance,
        overall_confidence,
    })
}

/// True for `{type: array, items: {type: object, properties}}`.
fn is_array_of_objects(prop: &Value) -> bool {
    prop.get("type").and_then(Value::as_str) == Some("array")
        && prop
            .get("items")
            .and_then(|i| i.get("type").and_then(Value::as_str))
            == Some("object")
}

/// Normalize a label for matching: lowercase alphanumeric words only.
fn normalize_label(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Candidate labels for a property: name plus `title`/`description` aliases.
fn property_aliases(name: &str, prop: &Value) -> Vec<String> {
    let mut aliases = vec![normalize_label(name)];
    for key in ["title", "description"] {
        if let Some(label) = prop.get(key).and_then(Value::as_str) {
            let norm = normalize_label(label);
            if !aliases.contains(&norm) {
                aliases.push(norm);
            }
        }
    }
    aliases
}

// ---------------------------------------------------------------------------
// Tier 1: AcroForm fields
// ---------------------------------------------------------------------------

fn match_acroform(
    doc: &ParsedDocument,
    name: &str,
    prop: &Value,
    target: &str,
) -> Option<SchemaFieldMatch> {
    let aliases = property_aliases(name, prop);
    doc.form_fields.iter().find_map(|field| {
        if !aliases.contains(&normalize_label(&field.name)) {
            return None;
        }
        let value = coerce_value(&field.value, target)?;
        Some(SchemaFieldMatch {
            field_name: name.to_string(),
            value,
            confidence: CONF_ACROFORM,
            page: field.page,
            bbox: field.bbox,
            source: "acroform".to_string(),
        })
    })
}

// ---------------------------------------------------------------------------
// Tier 2: inline `Label: value` / `Label - value` lines
// ---------------------------------------------------------------------------

/// Split a line into `(label, value)` on the first `:` or ` - ` separator.
fn split_inline_kv(line: &str) -> Option<(&str, &str)> {
    if let Some((label, value)) = line.split_once(':') {
        return Some((label, value));
    }
    line.split_once(" - ")
}

fn match_inline_kv(
    doc: &ParsedDocument,
    name: &str,
    prop: &Value,
    target: &str,
) -> Option<SchemaFieldMatch> {
    let aliases = property_aliases(name, prop);
    for cand in candidates(doc) {
        for line in cand.text.lines() {
            let line = line.trim().trim_start_matches(['#', '>', '-', '*', ' ']);
            let Some((label, value)) = split_inline_kv(line) else {
                continue;
            };
            if !aliases.contains(&normalize_label(label.trim())) {
                continue;
            }
            let coerced = coerce_value(value.trim(), target)?;
            return Some(SchemaFieldMatch {
                field_name: name.to_string(),
                value: coerced,
                confidence: CONF_INLINE_KV,
                page: cand.page,
                bbox: cand.bbox,
                source: "inline_kv".to_string(),
            });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tier 3: spatial right-of / below adjacency
// ---------------------------------------------------------------------------

/// One matchable text unit: a retained line when the assembler kept
/// them, else a whole chunk (hand-built documents).
struct Cand<'a> {
    idx: usize,
    page: u32,
    bbox: BBox,
    text: &'a str,
}

fn candidates(doc: &ParsedDocument) -> Vec<Cand<'_>> {
    if !doc.text_lines.is_empty() {
        doc.text_lines
            .iter()
            .enumerate()
            .map(|(idx, line)| Cand {
                idx,
                page: line.page,
                bbox: line.bbox,
                text: &line.text,
            })
            .collect()
    } else {
        doc.chunks
            .iter()
            .enumerate()
            .map(|(idx, chunk)| Cand {
                idx,
                page: chunk.page_index as u32,
                bbox: chunk.bbox,
                text: &chunk.markdown,
            })
            .collect()
    }
}

/// A candidate whose whole text is just the label (a lone caption
/// awaiting its value in a neighbor).
fn is_label_only(text: &str, aliases: &[String]) -> bool {
    let text = text.trim();
    !text.is_empty() && text.len() <= 80 && aliases.contains(&normalize_label(text))
}

/// Nearest value candidate right-of or below a label on the same page.
fn nearest_value<'a>(cands: &'a [Cand<'a>], label_idx: usize) -> Option<&'a Cand<'a>> {
    let label = &cands[label_idx];
    let line_h = label.bbox.height().max(1.0);
    let mut best: Option<(&Cand, f32)> = None;
    for cand in cands {
        if cand.idx == label.idx || cand.page != label.page {
            continue;
        }
        let text = cand.text.trim();
        if text.is_empty() {
            continue;
        }
        // Right-of: value starts at/after the label's right edge with
        // strong vertical overlap (same visual row).
        let right_of = cand.bbox.x0 >= label.bbox.x1 - 4.0
            && label.bbox.vertical_overlap_ratio(&cand.bbox) >= 0.5;
        // Below: value starts under the label within ~2 rows and aligns
        // horizontally (x-overlap with the label column).
        let gap = cand.bbox.y0 - label.bbox.y1;
        let below = gap >= 0.0
            && gap <= 1.8 * line_h
            && cand.bbox.x0 < label.bbox.x1
            && cand.bbox.x1 > label.bbox.x0;
        if !right_of && !below {
            continue;
        }
        let dist = if right_of {
            cand.bbox.x0 - label.bbox.x1
        } else {
            gap
        }
        .max(0.0);
        if best.is_none_or(|(_, d)| dist < d) {
            best = Some((cand, dist));
        }
    }
    best.map(|(cand, _)| cand)
}

fn match_spatial_kv(
    doc: &ParsedDocument,
    name: &str,
    prop: &Value,
    target: &str,
) -> Option<SchemaFieldMatch> {
    let aliases = property_aliases(name, prop);
    let cands = candidates(doc);
    for i in 0..cands.len() {
        if !is_label_only(cands[i].text, &aliases) {
            continue;
        }
        let value = nearest_value(&cands, i)?;
        let text = value.text.trim();
        // Skip table grids: tier 4 owns those.
        if text
            .lines()
            .next()
            .is_some_and(|l| l.trim_start().starts_with('|'))
        {
            continue;
        }
        let coerced = coerce_value(text, target)?;
        return Some(SchemaFieldMatch {
            field_name: name.to_string(),
            value: coerced,
            confidence: CONF_SPATIAL_KV,
            page: value.page,
            bbox: value.bbox,
            source: "spatial_kv".to_string(),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Tier 4: table header-to-row mapping for arrays of objects
// ---------------------------------------------------------------------------

/// Parse one Markdown table grid into (header, body rows) cell matrices.
fn parse_markdown_table(text: &str) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let rows: Vec<Vec<String>> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|'))
        .map(|l| {
            l.trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect()
        })
        .collect();
    if rows.len() < 3
        || !rows[1]
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ':'))
    {
        return None;
    }
    let header = rows[0].clone();
    let body = rows[2..].to_vec();
    if header.is_empty() || body.is_empty() {
        return None;
    }
    Some((header, body))
}

fn match_table_array(doc: &ParsedDocument, name: &str, prop: &Value) -> Option<SchemaFieldMatch> {
    let item_props = prop.get("items")?.get("properties")?.as_object()?;
    // Column index per item property, resolved against the table header.
    for chunk in &doc.chunks {
        let Some((header, body)) = parse_markdown_table(&chunk.markdown) else {
            continue;
        };
        let norm_header: Vec<String> = header.iter().map(|h| normalize_label(h)).collect();
        let mut columns: Vec<(&String, &Value, usize)> = Vec::new();
        let mut complete = true;
        for (prop_name, prop_schema) in item_props {
            match norm_header
                .iter()
                .position(|h| *h == normalize_label(prop_name))
            {
                Some(idx) => columns.push((prop_name, prop_schema, idx)),
                None => {
                    complete = false;
                    break;
                }
            }
        }
        if !complete || columns.is_empty() {
            continue;
        }
        let mut items = Vec::with_capacity(body.len());
        for row in &body {
            let mut obj = Map::new();
            for (prop_name, prop_schema, idx) in &columns {
                let cell = row.get(*idx).map(String::as_str).unwrap_or("");
                let target = prop_schema
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("string");
                let value = coerce_value(cell, target).unwrap_or(Value::String(cell.to_string()));
                obj.insert((*prop_name).clone(), value);
            }
            items.push(Value::Object(obj));
        }
        return Some(SchemaFieldMatch {
            field_name: name.to_string(),
            value: Value::Array(items),
            confidence: CONF_TABLE_COLUMN,
            page: chunk.page_index as u32,
            bbox: chunk.bbox,
            source: "table_column".to_string(),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Type coercion
// ---------------------------------------------------------------------------

/// Strip currency symbols, grouping commas, and whitespace for numbers.
fn clean_number(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '$' | '€' | '₹' | '£' | '¥' | ',' | '_' | ' ' | '\u{a0}'))
        .collect()
}

/// Coerce raw text to a JSON Schema scalar type (`None` = no match).
fn coerce_value(text: &str, target: &str) -> Option<Value> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match target {
        "number" => {
            let cleaned = clean_number(text);
            cleaned
                .parse::<f64>()
                .ok()
                .map(|n| serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number))
        }
        "integer" => {
            let cleaned = clean_number(text);
            cleaned
                .parse::<i64>()
                .ok()
                .map(|n| Value::Number(n.into()))
                .or_else(|| {
                    cleaned
                        .parse::<f64>()
                        .ok()
                        .and_then(|f| (f.fract() == 0.0).then(|| Value::Number((f as i64).into())))
                })
        }
        "boolean" => match text.to_lowercase().as_str() {
            "true" | "yes" | "y" | "1" | "on" | "checked" | "paid" | "x" => Some(Value::Bool(true)),
            "false" | "no" | "n" | "0" | "off" | "unchecked" | "unpaid" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => Some(Value::String(text.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BBox, FormFieldEntry, GroundedChunk, PageBBox};

    fn chunk(page: usize, y0: f32, y1: f32, markdown: &str) -> GroundedChunk {
        chunk_at(page, 72.0, y0, 540.0, y1, markdown)
    }

    fn chunk_at(page: usize, x0: f32, y0: f32, x1: f32, y1: f32, markdown: &str) -> GroundedChunk {
        let bbox = BBox::new(x0, y0, x1, y1);
        GroundedChunk {
            chunk_id: format!("p{page}-{x0:.0}-{y0:.0}"),
            page_index: page,
            bbox,
            breadcrumb: Vec::new(),
            markdown: markdown.to_string(),
            token_estimate: 10,
            parsed_by_vlm: false,
            page_spans: vec![PageBBox {
                page: page as u32,
                bbox,
            }],
            footnotes: Vec::new(),
            block_kind: "paragraph".to_string(),
            triage_score: 0.0,
        }
    }

    fn doc(chunks: Vec<GroundedChunk>, form_fields: Vec<FormFieldEntry>) -> ParsedDocument {
        ParsedDocument {
            source_path: "invoice.pdf".to_string(),
            total_pages: 1,
            fast_path_pages: 1,
            vlm_fallback_regions: 0,
            elapsed_ms: 1,
            markdown: String::new(),
            chunks,
            form_fields,
            footnotes: Vec::new(),
            text_lines: Vec::new(),
        }
    }

    fn field(name: &str, value: &str) -> FormFieldEntry {
        FormFieldEntry {
            name: name.to_string(),
            value: value.to_string(),
            field_type: "Tx".to_string(),
            bbox: BBox::new(100.0, 100.0, 200.0, 112.0),
            page: 0,
        }
    }

    fn invoice_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["invoice_number", "total_amount"],
            "properties": {
                "invoice_number": {"type": "string", "title": "Invoice Number"},
                "total_amount": {"type": "number", "title": "Total Amount"},
                "is_paid": {"type": "boolean", "title": "Paid"},
                "line_items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "item": {"type": "string"},
                            "qty": {"type": "integer"},
                            "price": {"type": "number"}
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn acroform_match_wins_with_high_confidence() {
        let doc = doc(vec![], vec![field("invoice_number", "INV-42")]);
        let result = extract_with_schema(&doc, &invoice_schema()).expect("extract");
        assert_eq!(result.data["invoice_number"], serde_json::json!("INV-42"));
        let hit = result
            .provenance
            .iter()
            .find(|m| m.field_name == "invoice_number")
            .expect("provenance");
        assert_eq!(hit.source, "acroform");
        assert!((hit.confidence - 0.98).abs() < 1e-6);
        assert_eq!(hit.page, 0);
    }

    #[test]
    fn acroform_matches_title_alias_case_insensitively() {
        let doc = doc(vec![], vec![field("Invoice Number", "INV-7")]);
        let result = extract_with_schema(&doc, &invoice_schema()).expect("extract");
        assert_eq!(result.data["invoice_number"], serde_json::json!("INV-7"));
    }

    #[test]
    fn inline_colon_value_coerces_number() {
        let doc = doc(
            vec![chunk(0, 100.0, 112.0, "Total Amount: $1,234.56")],
            vec![],
        );
        let result = extract_with_schema(&doc, &invoice_schema()).expect("extract");
        assert_eq!(
            result.data["total_amount"],
            serde_json::json!(1234.56),
            "data: {}",
            result.data
        );
    }

    #[test]
    fn inline_value_coerces_boolean() {
        let doc = doc(vec![chunk(0, 100.0, 112.0, "Paid: yes")], vec![]);
        let result = extract_with_schema(&doc, &invoice_schema()).expect("extract");
        assert_eq!(result.data["is_paid"], serde_json::json!(true));
    }

    #[test]
    fn spatial_right_of_chunk_finds_value() {
        let label = chunk_at(0, 72.0, 100.0, 200.0, 112.0, "Invoice Number");
        let value = chunk_at(0, 220.0, 100.0, 320.0, 112.0, "INV-99");
        let doc = doc(vec![label, value], vec![]);
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"invoice_number": {"type": "string"}}
        });
        let result = extract_with_schema(&doc, &schema).expect("extract");
        assert_eq!(result.data["invoice_number"], serde_json::json!("INV-99"));
        assert_eq!(result.provenance[0].source, "spatial_kv");
    }

    #[test]
    fn spatial_below_chunk_finds_value() {
        let label = chunk_at(0, 72.0, 100.0, 200.0, 112.0, "Invoice Number");
        let value = chunk_at(0, 72.0, 116.0, 200.0, 128.0, "INV-100");
        let doc = doc(vec![label, value], vec![]);
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"invoice_number": {"type": "string"}}
        });
        let result = extract_with_schema(&doc, &schema).expect("extract");
        assert_eq!(result.data["invoice_number"], serde_json::json!("INV-100"));
    }

    #[test]
    fn table_rows_map_to_array_of_objects() {
        let table = "| Item | Qty | Price |\n| --- | --- | --- |\n| Apples | 10 | 5.50 |\n| Pears | 4 | 3.25 |";
        let doc = doc(vec![chunk(0, 100.0, 200.0, table)], vec![]);
        let result = extract_with_schema(&doc, &invoice_schema()).expect("extract");
        let items = result.data["line_items"].as_array().expect("array");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["item"], serde_json::json!("Apples"));
        assert_eq!(items[0]["qty"], serde_json::json!(10));
        assert_eq!(items[0]["price"], serde_json::json!(5.50));
        let hit = result
            .provenance
            .iter()
            .find(|m| m.field_name == "line_items")
            .expect("provenance");
        assert_eq!(hit.source, "table_column");
    }

    #[test]
    fn missing_fields_omitted_and_confidence_zero_when_empty() {
        let doc = doc(
            vec![chunk(0, 100.0, 112.0, "Nothing relevant here.")],
            vec![],
        );
        let result = extract_with_schema(&doc, &invoice_schema()).expect("extract");
        assert!(result.data.get("invoice_number").is_none());
        assert_eq!(result.overall_confidence, 0.0);
    }

    #[test]
    fn overall_confidence_averages_hits() {
        let doc = doc(
            vec![chunk(0, 100.0, 112.0, "Total Amount: 42")],
            vec![field("invoice_number", "INV-1")],
        );
        let result = extract_with_schema(&doc, &invoice_schema()).expect("extract");
        let expected = (0.98 + 0.85) / 2.0;
        assert!((result.overall_confidence - expected).abs() < 1e-6);
    }

    #[test]
    fn non_object_schema_is_rejected() {
        let doc = doc(vec![], vec![]);
        assert!(extract_with_schema(&doc, &serde_json::json!([1, 2])).is_err());
        assert!(extract_with_schema(&doc, &serde_json::json!({"type": "object"})).is_err());
    }

    #[test]
    fn line_candidates_yield_tight_inline_bbox() {
        let coarse = BBox::new(0.0, 0.0, 600.0, 800.0);
        let tight = BBox::new(72.0, 100.0, 300.0, 112.0);
        let mut d = doc(
            vec![chunk_at(
                0,
                coarse.x0,
                coarse.y0,
                coarse.x1,
                coarse.y1,
                "Total Amount: 42.50",
            )],
            vec![],
        );
        d.text_lines = vec![crate::models::LineCandidate {
            text: "Total Amount: 42.50".to_string(),
            page: 0,
            bbox: tight,
        }];
        let result = extract_with_schema(&d, &invoice_schema()).expect("extract");
        let hit = result
            .provenance
            .iter()
            .find(|m| m.field_name == "total_amount")
            .expect("provenance");
        assert_eq!(hit.bbox, tight);
    }

    #[test]
    fn line_candidates_yield_tight_spatial_bbox() {
        let label_box = BBox::new(72.0, 100.0, 200.0, 112.0);
        let value_box = BBox::new(220.0, 100.0, 320.0, 112.0);
        let mut d = doc(
            vec![chunk_at(0, 0.0, 0.0, 600.0, 800.0, "Invoice Number")],
            vec![],
        );
        d.text_lines = vec![
            crate::models::LineCandidate {
                text: "Invoice Number".to_string(),
                page: 0,
                bbox: label_box,
            },
            crate::models::LineCandidate {
                text: "INV-99".to_string(),
                page: 0,
                bbox: value_box,
            },
        ];
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"invoice_number": {"type": "string"}}
        });
        let result = extract_with_schema(&d, &schema).expect("extract");
        assert_eq!(result.data["invoice_number"], serde_json::json!("INV-99"));
        assert_eq!(result.provenance[0].bbox, value_box);
    }
}
