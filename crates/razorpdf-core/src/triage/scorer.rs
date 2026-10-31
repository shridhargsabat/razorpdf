//! Smart triage & sub-page complexity scoring.
//!
//! Full `S(R)` implementation lands in Phase 3; this stub keeps the
//! pipeline API stable (fast path only).

use crate::models::{BBox, RawPageElements, TriageReason};

/// Triage verdict for a page or region.
#[derive(Debug, Clone)]
pub struct TriageScore {
    /// Complexity `S(R)` in `[0.0, 1.0]`.
    pub score: f32,
    /// Fallback reason when `score >= threshold`.
    pub reason: Option<TriageReason>,
    /// Padded fallback region bbox, if any.
    pub region: Option<BBox>,
}

/// Score a whole page (Phase 1-2: always fast path).
#[must_use]
pub fn score_page(_page: &RawPageElements) -> TriageScore {
    TriageScore {
        score: 0.0,
        reason: None,
        region: None,
    }
}
