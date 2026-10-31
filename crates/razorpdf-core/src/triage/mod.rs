//! Complexity scoring & sub-page region detection.

pub mod scorer;

pub use scorer::{
    complexity_score, diagram_clusters, entropy_runs, is_scanned_page, overlap_involvement,
    score_page, score_page_with_lines, EntropyRun, FallbackRegion, PageTriage,
};
