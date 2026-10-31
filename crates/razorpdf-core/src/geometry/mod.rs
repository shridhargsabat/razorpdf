//! Spatial indexing, XY-Cut reading order, deterministic table grids.

pub mod bbox;
pub mod lines;
pub mod table_grid;
pub mod xy_cut;

pub use lines::{TextLine, classify_heading, median_font_size, merge_spans_to_lines};
pub use table_grid::{DetectedTable, detect_vector_tables};
pub use xy_cut::{HasBBox, reading_order, sort_by_reading_order};
