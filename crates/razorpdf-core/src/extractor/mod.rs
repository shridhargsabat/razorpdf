//! PDFium/lopdf raw char, font, & vector extraction.

pub mod fonts;
pub mod lopdf_fallback;
pub mod pdfium_backend;
pub mod schema;

pub use lopdf_fallback::{extract_document, extract_page};
pub use schema::extract_with_schema;
