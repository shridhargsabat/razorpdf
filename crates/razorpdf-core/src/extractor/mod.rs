//! PDFium/lopdf raw char, font, & vector extraction.

pub mod fonts;
pub mod lopdf_fallback;
pub mod pdfium_backend;

pub use lopdf_fallback::{extract_document, extract_page};
