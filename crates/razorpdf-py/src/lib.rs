//! PyO3 Python bindings: `parse_file` and `to_markdown`.
//!
//! Rust parsing runs on a shared tokio runtime with the GIL released
//! (`detach`), so concurrent Python threads parse in parallel.

// `#[pyfunction]`-generated wrappers trip clippy::useless_conversion on
// `PyResult`; the lint cannot see into the macro expansion, so allow
// crate-wide (this crate is nothing but pyfunction shims).
#![allow(clippy::useless_conversion)]

use std::sync::{Arc, OnceLock};

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use razorpdf_core::{
    assembler,
    vlm::{cropper::CanvasSource, HttpVlmProvider, NoVlmProvider},
    BBox, ParsedDocument, ParserConfig,
};

/// One BM25 hit, serialized to a Python dict via `pythonize`.
#[derive(serde::Serialize)]
struct ChunkHit<'a> {
    chunk_id: &'a str,
    page: usize,
    bbox: &'a BBox,
    section_path: &'a [String],
    score: f32,
    markdown: &'a str,
}

/// Parse the `canvas_source` keyword (`None`/`"synthetic"`/`"auto"`).
fn parse_canvas_source(canvas_source: Option<&str>) -> PyResult<Option<CanvasSource>> {
    match canvas_source {
        None | Some("synthetic") => Ok(None),
        Some("auto") => Ok(Some(CanvasSource::Auto)),
        Some(other) => Err(PyValueError::new_err(format!(
            "canvas_source must be 'synthetic' or 'auto', got '{other}'"
        ))),
    }
}

/// Shared tokio runtime for blocking on the async assembler.
fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RT: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Runtime::new().map_err(|e| format!("tokio runtime: {e}")))
        .as_ref()
        .map_err(|e| e.clone())
}

/// Assemble a document synchronously (no Python types touched: safe to call
/// without the GIL).
fn assemble_sync(
    path: &str,
    vlm_url: Option<&str>,
    canvas_source: Option<CanvasSource>,
    stitch_pages: bool,
    use_cache: bool,
) -> Result<ParsedDocument, String> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let mut config = ParserConfig {
            enable_cross_page_stitching: stitch_pages,
            ..ParserConfig::default()
        };
        config.vlm.enable_vlm_cache = use_cache;
        if let Some(url) = vlm_url {
            config.vlm.url = Some(url.to_string());
        }
        if let Some(canvas) = canvas_source {
            config.vlm.canvas_source = canvas;
        }
        let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        if config.vlm.url.is_some() || std::env::var("RAZORPDF_VLM_URL").is_ok() {
            let http = HttpVlmProvider::new(config.vlm.clone()).map_err(|e| e.to_string())?;
            assembler::assemble_document(&bytes, path, &config, Arc::new(http))
                .await
                .map_err(|e| e.to_string())
        } else {
            assembler::assemble_document(&bytes, path, &config, Arc::new(NoVlmProvider))
                .await
                .map_err(|e| e.to_string())
        }
    })
}

/// Parse a PDF file into a grounded document dict.
///
/// Keys: `source_path`, `total_pages`, `fast_path_pages`,
/// `vlm_fallback_regions`, `elapsed_ms`, `markdown`, `chunks` (each with
/// `chunk_id`, `page_index`, `bbox`, `breadcrumb`, `markdown`,
/// `token_estimate`, `parsed_by_vlm`).
#[pyfunction]
#[pyo3(signature = (path, vlm_url=None, canvas_source=None, stitch_pages=true, use_cache=true))]
fn parse_file(
    py: Python<'_>,
    path: &str,
    vlm_url: Option<&str>,
    canvas_source: Option<&str>,
    stitch_pages: bool,
    use_cache: bool,
) -> PyResult<Py<PyAny>> {
    let path = path.to_string();
    let vlm_url = vlm_url.map(str::to_string);
    let canvas = parse_canvas_source(canvas_source)?;
    let doc = py
        .detach(|| assemble_sync(&path, vlm_url.as_deref(), canvas, stitch_pages, use_cache))
        .map_err(PyRuntimeError::new_err)?;
    Ok(pythonize::pythonize(py, &doc)?.unbind())
}

/// Convert a PDF file to Markdown.
#[pyfunction]
#[pyo3(signature = (path, stitch_pages=true, use_cache=true))]
fn to_markdown(
    py: Python<'_>,
    path: &str,
    stitch_pages: bool,
    use_cache: bool,
) -> PyResult<String> {
    let path = path.to_string();
    let doc = py
        .detach(|| assemble_sync(&path, None, None, stitch_pages, use_cache))
        .map_err(PyRuntimeError::new_err)?;
    Ok(doc.markdown)
}

/// BM25 + phrase-match retrieval over a PDF's grounded chunks.
///
/// Returns a list of `{chunk_id, page, bbox, section_path, score,
/// markdown}` dicts, best first. `page` is 1-based.
#[pyfunction]
#[pyo3(signature = (path, query, top_k=5))]
fn search_chunks(py: Python<'_>, path: &str, query: &str, top_k: usize) -> PyResult<Py<PyAny>> {
    if query.trim().is_empty() {
        return Err(PyValueError::new_err("query must not be empty"));
    }
    let path = path.to_string();
    let query = query.to_string();
    let (chunks, ranked) = py
        .detach(|| {
            let doc = assemble_sync(&path, None, None, true, true)?;
            let ranked = razorpdf_core::search::bm25_rank(&doc.chunks, &query, top_k.max(1));
            Ok::<_, String>((doc.chunks, ranked))
        })
        .map_err(PyRuntimeError::new_err)?;
    let hits: Vec<ChunkHit> = ranked
        .into_iter()
        .map(|(i, score)| {
            let chunk = &chunks[i];
            ChunkHit {
                chunk_id: &chunk.chunk_id,
                page: chunk.page_index + 1,
                bbox: &chunk.bbox,
                section_path: &chunk.breadcrumb,
                score,
                markdown: &chunk.markdown,
            }
        })
        .collect();
    Ok(pythonize::pythonize(py, &hits)?.unbind())
}

/// Render a highlighted citation crop of a page region as WebP bytes.
///
/// `page` is 1-based; `bbox` is `[x0, y0, x1, y1]` in PDF points.
#[pyfunction]
#[pyo3(signature = (path, page, bbox, canvas_source=None))]
fn render_citation(
    py: Python<'_>,
    path: &str,
    page: u32,
    bbox: Vec<f32>,
    canvas_source: Option<&str>,
) -> PyResult<Vec<u8>> {
    if page < 1 {
        return Err(PyValueError::new_err("page is 1-based"));
    }
    if bbox.len() != 4 {
        return Err(PyValueError::new_err("bbox must be [x0, y0, x1, y1]"));
    }
    let (x0, y0, x1, y1) = (bbox[0], bbox[1], bbox[2], bbox[3]);
    if !(x0 < x1 && y0 < y1) {
        return Err(PyValueError::new_err(
            "bbox must satisfy x0 < x1 and y0 < y1",
        ));
    }
    let canvas = parse_canvas_source(canvas_source)?;
    let path = path.to_string();
    py.detach(|| render_citation_sync(&path, page, BBox::new(x0, y0, x1, y1), canvas))
        .map_err(PyRuntimeError::new_err)
}

/// Render path shared by `render_citation` (no Python types: GIL-free).
fn render_citation_sync(
    path: &str,
    page_1based: u32,
    bbox: BBox,
    canvas_source: Option<CanvasSource>,
) -> Result<Vec<u8>, String> {
    use razorpdf_core::{extractor, vlm::cropper};
    let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let pages = extractor::extract_document(&bytes, path).map_err(|e| e.to_string())?;
    let page_data = pages
        .get(page_1based as usize - 1)
        .ok_or_else(|| format!("page {page_1based} out of range (1..={})", pages.len()))?;
    let canvas = cropper::render_page_canvas(
        &bytes,
        page_data,
        150,
        canvas_source.unwrap_or(CanvasSource::Synthetic),
    );
    cropper::render_citation_overlay(&canvas, page_data.width, page_data.height, &bbox, 20.0, 80)
        .map_err(|e| e.to_string())
}

/// Python module definition.
#[pymodule]
fn razorpdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_file, m)?)?;
    m.add_function(wrap_pyfunction!(to_markdown, m)?)?;
    m.add_function(wrap_pyfunction!(search_chunks, m)?)?;
    m.add_function(wrap_pyfunction!(render_citation, m)?)?;
    Ok(())
}
