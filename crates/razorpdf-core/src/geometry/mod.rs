//! Spatial indexing, XY-Cut reading order, deterministic table grids.

pub mod bbox;
pub mod lines;
pub mod table_grid;
pub mod xy_cut;

pub use lines::{
    body_font_size, classify_heading, median_font_size, merge_spans_to_lines, TextLine,
};
pub use table_grid::{
    detect_all_tables, detect_borderless_tables, detect_ruled_tables, detect_vector_tables,
    DetectedTable, TableSource,
};
pub use xy_cut::{reading_order, sort_by_reading_order, HasBBox};
