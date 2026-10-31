//! Grounded RAG chunk helpers.
//!
//! Minimal live implementation (token heuristic); breadcrumb tracking and
//! full chunk synthesis complete in Phase 4.

/// Heuristic token estimate (~4 chars per token).
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_scales_with_length() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }
}
