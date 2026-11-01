//! Adversarial robustness: malformed PDFs must error, never panic/hang/OOM.
//!
//! Each test proves a bug found in the security audit (or locks in safe
//! behavior for an audited edge). Bugs fixed: decompression bombs,
//! ToUnicode CMap u32 overflows, synthetic-canvas dimension bombs, and
//! unescaped carriage returns in Markdown table cells.

use flate2::write::ZlibEncoder;
use flate2::Compression;
use lopdf::{dictionary, Document, Object, Stream};
use razorpdf_core::extractor::extract_document;
use razorpdf_core::geometry::{table_grid::detected_table_to_markdown, DetectedTable, TableSource};
use razorpdf_core::models::BBox;
use std::io::Write as _;

// ---------------------------------------------------------------------------
// Minimal adversarial PDF builder (lopdf direct; full control over streams)
// ---------------------------------------------------------------------------

struct FontDef {
    name: &'static str,
    to_unicode: Option<&'static [u8]>,
}

struct FormDef {
    name: &'static str,
    content: &'static str,
}

struct PageDef {
    content: Vec<u8>,
    filter: Option<&'static str>,
    mediabox: [f32; 4],
    /// Extra content streams (makes `/Contents` an array).
    extra: Vec<(Vec<u8>, Option<&'static str>)>,
}

fn build_pdf(pages: Vec<PageDef>, fonts: Vec<FontDef>, forms: Vec<FormDef>) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");

    let mut font_map = lopdf::Dictionary::new();
    for spec in &fonts {
        let mut font_dict = lopdf::Dictionary::new();
        font_dict.set("Type", Object::Name(b"Font".to_vec()));
        font_dict.set("Subtype", Object::Name(b"Type1".to_vec()));
        font_dict.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
        if let Some(cmap) = spec.to_unicode {
            let cmap_id =
                doc.add_object(Object::Stream(Stream::new(dictionary! {}, cmap.to_vec())));
            font_dict.set("ToUnicode", Object::Reference(cmap_id));
        }
        let font_id = doc.add_object(Object::Dictionary(font_dict));
        font_map.set(spec.name, Object::Reference(font_id));
    }

    // Forms first (empty Resources), then patch to the shared resource dict
    // so forms can reference each other (cycles expressible).
    let mut form_ids = Vec::new();
    for spec in &forms {
        let mut form_dict = lopdf::Dictionary::new();
        form_dict.set("Type", Object::Name(b"XObject".to_vec()));
        form_dict.set("Subtype", Object::Name(b"Form".to_vec()));
        form_dict.set(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        let id = doc.add_object(Object::Stream(Stream::new(
            form_dict,
            spec.content.as_bytes().to_vec(),
        )));
        form_ids.push(id);
    }
    let mut resources = lopdf::Dictionary::new();
    resources.set("Font", Object::Dictionary(font_map));
    if !forms.is_empty() {
        let mut xmap = lopdf::Dictionary::new();
        for (spec, id) in forms.iter().zip(form_ids.iter()) {
            xmap.set(spec.name, Object::Reference(*id));
        }
        resources.set("XObject", Object::Dictionary(xmap));
        for id in &form_ids {
            if let Ok(Object::Stream(stream)) = doc.get_object_mut(*id) {
                stream
                    .dict
                    .set("Resources", Object::Dictionary(resources.clone()));
            }
        }
    }

    let mut page_ids = Vec::new();
    for page in pages {
        let mut content_refs = Vec::new();
        let mut streams = vec![(page.content, page.filter)];
        streams.extend(page.extra);
        for (bytes, filter) in streams {
            let mut dict = dictionary! {};
            if let Some(filter) = filter {
                dict.set("Filter", Object::Name(filter.as_bytes().to_vec()));
            }
            let content_id = doc.add_object(Object::Stream(Stream::new(dict, bytes)));
            content_refs.push(Object::Reference(content_id));
        }
        let contents = if content_refs.len() == 1 {
            content_refs.into_iter().next().unwrap()
        } else {
            Object::Array(content_refs)
        };
        let mut paged = lopdf::Dictionary::new();
        paged.set("Type", Object::Name(b"Page".to_vec()));
        paged.set(
            "MediaBox",
            Object::Array(page.mediabox.iter().map(|m| Object::Real(*m)).collect()),
        );
        paged.set("Resources", Object::Dictionary(resources.clone()));
        paged.set("Contents", contents);
        page_ids.push(doc.add_object(Object::Dictionary(paged)));
    }

    let mut pages_node = lopdf::Dictionary::new();
    pages_node.set("Type", Object::Name(b"Pages".to_vec()));
    pages_node.set(
        "Kids",
        Object::Array(page_ids.iter().map(|id| Object::Reference(*id)).collect()),
    );
    pages_node.set("Count", Object::Integer(page_ids.len() as i64));
    let pages_id = doc.add_object(Object::Dictionary(pages_node));
    for page_id in &page_ids {
        if let Ok(Object::Dictionary(d)) = doc.get_object_mut(*page_id) {
            d.set("Parent", Object::Reference(pages_id));
        }
    }
    let mut catalog = lopdf::Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    let mut buf = Vec::new();
    doc.save_to(&mut buf).expect("fixture serialize");
    buf
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

const LETTER: [f32; 4] = [0.0, 0.0, 612.0, 792.0];

// ---------------------------------------------------------------------------
// Bug 1: decompression bombs (content streams)
// ---------------------------------------------------------------------------

#[test]
fn flate_bomb_content_stream_is_rejected() {
    // 48 MiB of zeros compresses to ~50 KiB: unbounded inflate would
    // happily allocate it all. Must error instead.
    let bomb = zlib(&vec![0u8; 48 * 1024 * 1024]);
    assert!(bomb.len() < 1024 * 1024, "bomb should compress small");
    let pdf = build_pdf(
        vec![PageDef {
            content: bomb,
            filter: Some("FlateDecode"),
            mediabox: LETTER,
            extra: vec![],
        }],
        vec![FontDef {
            name: "F1",
            to_unicode: None,
        }],
        vec![],
    );
    let err = extract_document(&pdf, "bomb.pdf").expect_err("bomb must be rejected");
    assert!(
        format!("{err:?}").contains("exceed"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn contents_array_total_is_capped() {
    // Six 12 MiB streams: each under the per-stream cap, 72 MiB total over it.
    let chunk = zlib(&vec![0u8; 12 * 1024 * 1024]);
    let pdf = build_pdf(
        vec![PageDef {
            content: chunk.clone(),
            filter: Some("FlateDecode"),
            mediabox: LETTER,
            extra: vec![(chunk.clone(), Some("FlateDecode")); 5],
        }],
        vec![FontDef {
            name: "F1",
            to_unicode: None,
        }],
        vec![],
    );
    let err = extract_document(&pdf, "multi.pdf").expect_err("over-cap total must fail");
    assert!(
        format!("{err:?}").contains("exceed"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn legit_flate_content_still_parses() {
    let content = b"BT /F1 11 Tf 1 0 0 1 72 700 Tm (Hello bomb-free world) Tj ET\n";
    let pdf = build_pdf(
        vec![PageDef {
            content: zlib(content),
            filter: Some("FlateDecode"),
            mediabox: LETTER,
            extra: vec![],
        }],
        vec![FontDef {
            name: "F1",
            to_unicode: None,
        }],
        vec![],
    );
    let pages = extract_document(&pdf, "ok.pdf").expect("legit flate must parse");
    assert_eq!(pages.len(), 1);
    assert!(pages[0].spans.iter().any(|s| s.text.contains("Hello")));
}

// ---------------------------------------------------------------------------
// Bug 2: ToUnicode CMap u32 overflows (debug panics / release wraparounds)
// ---------------------------------------------------------------------------

#[test]
fn cmap_overlong_hex_does_not_panic() {
    use razorpdf_core::extractor::fonts::{parse_tounicode_cmap, FontDecoding};

    // 6-byte source code with high bits set. `<<` discards high bits
    // without panicking (only the shift *amount* can overflow), so this
    // locks in the wrapping semantic: low 4 bytes win.
    let cmap = parse_tounicode_cmap(b"1 beginbfchar\n<FF0000000000> <0041>\nendbfchar");
    let mut dec = FontDecoding::with_base_encoding("Standard");
    dec.apply_tounicode(&cmap);
    assert_eq!(dec.decode(b"\x00"), "A");
}

#[test]
fn cmap_bfrange_lookup_overflow_does_not_panic() {
    use razorpdf_core::extractor::fonts::{parse_tounicode_cmap, FontDecoding};

    // base + (code - lo) = 0xFFFFFFFF + 1 overflows u32 in debug.
    // Fixed semantic: destination wraps modulo 2^32.
    let cmap = parse_tounicode_cmap(b"1 beginbfrange\n<00> <FF> <FFFFFFFF>\nendbfrange");
    let mut dec = FontDecoding::with_base_encoding("Standard");
    dec.apply_tounicode(&cmap);
    let _ = dec.decode(b"\x01");
}

#[test]
fn cmap_lone_delimiters_terminate_with_bounded_tokens() {
    use razorpdf_core::extractor::fonts::parse_tounicode_cmap;

    // Fuzzer find: a lone `>`, `)`, `{`, `}` fell into the word arm with
    // zero bytes consumed, pushing empty words forever (4GB+ token vec,
    // host OOM, gateway kill). Any of these must return promptly.
    for junk in [
        b">>>))){{{}}}".as_slice(),
        b">",
        b")",
        b"1 beginbfchar\n> ) <0041>\nendbfchar",
        b"\x00\x01\x02><(){}[]/%\xff\xfe",
    ] {
        let cmap = parse_tounicode_cmap(junk);
        assert!(
            cmap.direct.len() + cmap.range_rules.len() <= junk.len(),
            "unbounded CMap output for {junk:?}"
        );
    }
}

#[test]
fn cmap_huge_section_count_returns_promptly() {
    use razorpdf_core::extractor::fonts::parse_tounicode_cmap;
    use std::time::{Duration, Instant};

    // HG-01: `for _ in 0..count` iterated an attacker-controlled count
    // (up to 9.2e18) doing O(1) probes past the token end: ~30 bytes of
    // CMap burned hours of CPU. Fixed by clamping count to remaining
    // tokens (every arm advances i by >= 2, so count > rest/2 is pure burn).
    let t = Instant::now();
    let cmap = parse_tounicode_cmap(b"1000000000 beginbfchar\n<41> <0041>\nendbfchar");
    assert!(t.elapsed() < Duration::from_secs(5), "section-count burn");
    assert_eq!(cmap.direct.len(), 1, "non-vacuous: mapping must exist");
}

#[test]
fn cmap_range_rule_width_is_capped() {
    use razorpdf_core::extractor::fonts::{parse_tounicode_cmap, FontDecoding};

    // HG-02: range-rule width came from the raw dst token length, and
    // int_to_utf16 allocates `width` bytes PER DECODED CHAR: a 200KB dst
    // turned 64 input bytes into 19.2MB of output. ToUnicode needs at
    // most 4 bytes/char (surrogate pair), so width is capped at 4.
    let big_hex = "AB".repeat(200_000);
    let src = format!("1 beginbfrange\n<00> <FF> <{big_hex}>\nendbfrange");
    let cmap = parse_tounicode_cmap(src.as_bytes());
    let mut dec = FontDecoding::default();
    dec.apply_tounicode(&cmap);
    let out = dec.decode(&[0x41; 64]);
    assert_eq!(cmap.range_rules.len(), 1, "non-vacuous: rule must parse");
    assert!(
        out.len() < 64 * 8,
        "width amplification: 64B in, {}B out",
        out.len()
    );
}

#[test]
fn cmap_bfrange_max_literal_does_not_panic() {
    use razorpdf_core::extractor::fonts::parse_tounicode_cmap;

    // HG-08: `hi.min(lo + 4095)` overflowed for lo near u32::MAX: debug
    // builds panicked, release wrapped to an empty range and silently
    // dropped the mapping. saturating_add unifies both profiles.
    let cmap = parse_tounicode_cmap(b"1 beginbfrange\n<FFFFFFFF> <FFFFFFFF> (X)\nendbfrange");
    assert_eq!(cmap.direct.get(&u32::MAX).map(String::as_str), Some("X"));
    let cmap2 = parse_tounicode_cmap(b"1 beginbfrange\n<FFFFFF9B> <FFFFFFFF> (Y)\nendbfrange");
    assert_eq!(cmap2.direct.len(), 101, "MAX-100..=MAX must map fully");
}

#[test]
fn cmap_bfrange_array_overflow_does_not_panic() {
    use razorpdf_core::extractor::fonts::parse_tounicode_cmap;

    // Array form: lo + step overflows at step 2 during CMap parsing.
    let cmap = parse_tounicode_cmap(
        b"1 beginbfrange\n<FFFFFFFE> <FFFFFFFF> [<0041> <0042> <0043>]\nendbfrange",
    );
    assert_eq!(cmap.direct.len(), 3, "all three array entries must land");
}

// ---------------------------------------------------------------------------
// Bug 3: synthetic-canvas dimension bomb (MediaBox 1e30 -> u32::MAX alloc)
// ---------------------------------------------------------------------------

#[test]
fn giant_mediabox_canvas_is_clamped() {
    use razorpdf_core::vlm::cropper::render_synthetic_canvas;

    // 1e7pt MediaBox parses fine (1e30 makes lopdf drop the page object),
    // but 2e7px/side at 150dpi would abort on allocation without a clamp.
    let pdf = build_pdf(
        vec![PageDef {
            content: b"BT /F1 11 Tf 1 0 0 1 72 700 Tm (tiny) Tj ET\n".to_vec(),
            filter: None,
            mediabox: [0.0, 0.0, 1e7, 1e7],
            extra: vec![],
        }],
        vec![FontDef {
            name: "F1",
            to_unicode: None,
        }],
        vec![],
    );
    let pages = extract_document(&pdf, "giant.pdf").expect("extract must survive");
    let img = render_synthetic_canvas(&pages[0], 150);
    let pixels = u64::from(img.width()) * u64::from(img.height());
    assert!(
        pixels <= 16_777_216,
        "canvas must be clamped, got {}x{}",
        img.width(),
        img.height()
    );
}

// ---------------------------------------------------------------------------
// Bug 4: carriage returns break Markdown table rows
// ---------------------------------------------------------------------------

#[test]
fn carriage_returns_in_table_cells_are_escaped() {
    let table = DetectedTable {
        bbox: BBox::new(0.0, 0.0, 100.0, 50.0),
        rows: vec![
            vec!["a\rb".to_string(), "c\r\nd".to_string()],
            vec!["plain".to_string(), "x\ny".to_string()],
        ],
        has_header: false,
        source: TableSource::VectorGrid,
    };
    let md = detected_table_to_markdown(&table);
    assert!(
        !md.contains('\r'),
        "raw CR leaks into table markdown: {md:?}"
    );
    assert_eq!(md.lines().count(), 4, "CR must not add rows: {md:?}");
}

// ---------------------------------------------------------------------------
// Audited-safe: regression locks (cyclic XObjects, NaN sorts, bare ops)
// ---------------------------------------------------------------------------

#[test]
fn cyclic_form_xobjects_terminate() {
    // FA <-> FB cycle: collection visited-set + runtime depth cap bound it.
    let pdf = build_pdf(
        vec![PageDef {
            content: b"/FA Do\n".to_vec(),
            filter: None,
            mediabox: LETTER,
            extra: vec![],
        }],
        vec![FontDef {
            name: "F1",
            to_unicode: None,
        }],
        vec![
            FormDef {
                name: "FA",
                content: "BT /F1 11 Tf 1 0 0 1 72 700 Tm (A) Tj ET\n/FB Do\n",
            },
            FormDef {
                name: "FB",
                content: "BT /F1 11 Tf 1 0 0 1 72 680 Tm (B) Tj ET\n/FA Do\n",
            },
        ],
    );
    let pages = extract_document(&pdf, "cycle.pdf").expect("cycles must terminate");
    let texts: Vec<&str> = pages[0].spans.iter().map(|s| s.text.as_str()).collect();
    assert!(texts.len() <= 4, "cycle must stay bounded, got {texts:?}");
    assert!(texts.contains(&"A") && texts.contains(&"B"));
}

#[test]
fn self_referential_form_terminates() {
    // Direct self-cycle: /FS Do inside FS.
    let pdf = build_pdf(
        vec![PageDef {
            content: b"/FS Do\n".to_vec(),
            filter: None,
            mediabox: LETTER,
            extra: vec![],
        }],
        vec![FontDef {
            name: "F1",
            to_unicode: None,
        }],
        vec![FormDef {
            name: "FS",
            content: "BT /F1 11 Tf 1 0 0 1 72 700 Tm (S) Tj ET\n/FS Do\n",
        }],
    );
    let pages = extract_document(&pdf, "self.pdf").expect("self-cycle must terminate");
    assert!(!pages[0].spans.is_empty());
}

#[test]
fn nan_and_extreme_coords_do_not_crash_sort() {
    use razorpdf_core::geometry::sort_by_reading_order;
    use razorpdf_core::ParserConfig;

    let boxes = vec![
        BBox {
            x0: f32::NAN,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        },
        BBox {
            x0: f32::INFINITY,
            y0: f32::NEG_INFINITY,
            x1: f32::MAX,
            y1: 5.0,
        },
        BBox::new(10.0, 10.0, 20.0, 20.0),
    ];
    let config = ParserConfig::default();
    let ordered = sort_by_reading_order(&boxes, 11.0, 13.0, &config);
    assert_eq!(ordered.len(), 3, "all items must survive the sort");
}

#[test]
fn bare_operators_without_operands_do_not_panic() {
    let mut content = b"Tf\nTm\nTd\nTD\nm\nl\nre\nDo\ncm\nw\nBT ET\n'\"\n".to_vec();
    content.extend_from_slice(b"BT /F1 11 Tf 1 0 0 1 72 700 Tm (survivor) Tj ET\n");
    let pdf = build_pdf(
        vec![PageDef {
            content,
            filter: None,
            mediabox: LETTER,
            extra: vec![],
        }],
        vec![FontDef {
            name: "F1",
            to_unicode: None,
        }],
        vec![],
    );
    let pages = extract_document(&pdf, "bare.pdf").expect("bare ops must not panic");
    assert!(pages[0].spans.iter().any(|s| s.text == "survivor"));
}

#[test]
fn garbage_bytes_error_cleanly() {
    assert!(extract_document(b"not a pdf at all", "junk.pdf").is_err());
    assert!(extract_document(b"", "empty.pdf").is_err());
    assert!(extract_document(b"%PDF-1.7\ntrailerless", "short.pdf").is_err());
}

#[test]
fn destroyed_catalog_errors_instead_of_empty_success() {
    // Fuzzer find: a splice that wipes the catalog object used to parse as
    // zero pages with exit 0 and empty output (silent data loss). Same
    // length replacement keeps xref offsets valid, isolating the catalog.
    let pdf = build_pdf(
        vec![PageDef {
            content: b"BT /F1 11 Tf 1 0 0 1 72 700 Tm (invoice) Tj ET\n".to_vec(),
            filter: None,
            mediabox: LETTER,
            extra: vec![],
        }],
        vec![FontDef {
            name: "F1",
            to_unicode: None,
        }],
        vec![],
    );
    // lopdf serializes dicts without spaces: `<</Type/Catalog`. Kill the
    // catalog's opening `<<` (same length, xref intact): the object becomes
    // unparseable and lopdf drops it, exactly like the fuzzer's splice.
    let needle = b"/Type/Catalog";
    let mut broken = pdf.clone();
    let mut mangled = false;
    for (i, w) in pdf.windows(needle.len()).enumerate() {
        if w == needle {
            broken[i - 2] = b'X';
            broken[i - 1] = b'X';
            mangled = true;
        }
    }
    assert!(mangled, "fixture must actually mangle the catalog");
    let err = extract_document(&broken, "nocat.pdf").expect_err("must not succeed");
    assert!(
        format!("{err:?}").to_lowercase().contains("page"),
        "unexpected error: {err:?}"
    );
}
