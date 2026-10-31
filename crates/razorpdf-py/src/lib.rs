//! PyO3 zero-copy Python bindings (full implementation: Phase 5).

use pyo3::prelude::*;

/// Parse a PDF file into a grounded JSON document (Phase 5).
#[pyfunction]
#[pyo3(signature = (path, vlm_url=None))]
fn parse_file(path: &str, vlm_url: Option<&str>) -> PyResult<String> {
    let _ = (path, vlm_url);
    Err(pyo3::exceptions::PyNotImplementedError::new_err(
        "razorpdf.parse_file ships in Phase 5",
    ))
}

/// Convert a PDF file to Markdown (Phase 5).
#[pyfunction]
fn to_markdown(path: &str) -> PyResult<String> {
    let _ = path;
    Err(pyo3::exceptions::PyNotImplementedError::new_err(
        "razorpdf.to_markdown ships in Phase 5",
    ))
}

/// Python module definition.
#[pymodule]
fn razorpdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_file, m)?)?;
    m.add_function(wrap_pyfunction!(to_markdown, m)?)?;
    Ok(())
}
