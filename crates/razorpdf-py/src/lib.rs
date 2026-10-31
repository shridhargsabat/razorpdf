//! PyO3 Python bindings: `parse_file` and `to_markdown`.
//!
//! Rust parsing runs on a shared tokio runtime with the GIL released
//! (`allow_threads`), so concurrent Python threads parse in parallel.

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
    ParsedDocument, ParserConfig,
};

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
) -> Result<ParsedDocument, String> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let mut config = ParserConfig::default();
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
#[pyo3(signature = (path, vlm_url=None, canvas_source=None))]
fn parse_file(
    py: Python<'_>,
    path: &str,
    vlm_url: Option<&str>,
    canvas_source: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let path = path.to_string();
    let vlm_url = vlm_url.map(str::to_string);
    let canvas = match canvas_source {
        None | Some("synthetic") => None,
        Some("auto") => Some(CanvasSource::Auto),
        Some(other) => {
            return Err(PyValueError::new_err(format!(
                "canvas_source must be 'synthetic' or 'auto', got '{other}'"
            )));
        }
    };
    let doc = py
        .allow_threads(|| assemble_sync(&path, vlm_url.as_deref(), canvas))
        .map_err(PyRuntimeError::new_err)?;
    Ok(pythonize::pythonize(py, &doc)?.unbind())
}

/// Convert a PDF file to Markdown.
#[pyfunction]
fn to_markdown(py: Python<'_>, path: &str) -> PyResult<String> {
    let path = path.to_string();
    let doc = py
        .allow_threads(|| assemble_sync(&path, None, None))
        .map_err(PyRuntimeError::new_err)?;
    Ok(doc.markdown)
}

/// Python module definition.
#[pymodule]
fn razorpdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_file, m)?)?;
    m.add_function(wrap_pyfunction!(to_markdown, m)?)?;
    Ok(())
}
