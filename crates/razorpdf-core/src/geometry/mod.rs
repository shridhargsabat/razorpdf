//! Spatial indexing, XY-Cut reading order, deterministic table grids.

pub mod bbox;
pub mod lines;
pub mod table_grid;
pub mod xy_cut;

pub use lines::{classify_heading, median_font_size, merge_spans_to_lines, TextLine};
pub use table_grid::{detect_vector_tables, DetectedTable};
pub use xy_cut::{reading_order, sort_by_reading_order, HasBBox};
