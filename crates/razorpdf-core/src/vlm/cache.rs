//! Content-addressed disk cache for VLM completions.
//!
//! Sub-page crops are deterministic for a given PDF + config, so identical
//! `(model, prompt, image)` triples return identical Markdown. Entries live
//! at `<dir>/<first-2-hex>/<full-hex>.json` and are written atomically.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::vlm::client::VlmResponse;

/// Domain separator + version prefix for cache keys.
const KEY_DOMAIN: &[u8] = b"razorpdf-vlm-v1:";

/// Content key: `blake3("razorpdf-vlm-v1:" + model + prompt + image)`.
///
/// Callers pass the raw WebP bytes, or the data-URI bytes (equivalent:
/// the URI deterministically wraps the bytes).
#[must_use]
pub fn compute_vlm_cache_key(image_bytes: &[u8], model: &str, prompt: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(KEY_DOMAIN);
    hasher.update(model.as_bytes());
    hasher.update(b":");
    hasher.update(prompt.as_bytes());
    hasher.update(b":");
    hasher.update(image_bytes);
    hasher.finalize().to_hex().to_string()
}

/// Disk-backed content-addressed VLM response cache.
#[derive(Debug, Clone)]
pub struct Blake3VlmCache {
    dir: PathBuf,
}

impl Blake3VlmCache {
    /// Open a cache rooted at `dir` (created lazily on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Resolve the cache directory: explicit override, `RAZORPDF_CACHE_DIR`
    /// env var, else `~/.cache/razorpdf/vlm`.
    #[must_use]
    pub fn resolve_dir(override_dir: Option<&Path>) -> PathBuf {
        if let Some(dir) = override_dir {
            return dir.to_path_buf();
        }
        if let Ok(dir) = std::env::var("RAZORPDF_CACHE_DIR") {
            return PathBuf::from(dir);
        }
        home_dir().join(".cache").join("razorpdf").join("vlm")
    }

    /// Keys are always 64 lowercase hex chars; anything else is rejected
    /// so adversarial key strings cannot escape the cache dir.
    fn valid_key(key: &str) -> bool {
        key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
    }

    fn path_for(&self, key: &str) -> Option<PathBuf> {
        if !Self::valid_key(key) {
            return None;
        }
        Some(self.dir.join(&key[..2]).join(format!("{key}.json")))
    }

    /// Fetch a cached response (`None` on miss, corrupt entry, or bad key).
    pub fn get(&self, key: &str) -> Option<VlmResponse> {
        let path = self.path_for(key)?;
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Store a response atomically (temp file + rename).
    pub fn put(&self, key: &str, response: &VlmResponse) -> Result<()> {
        let path = self.path_for(key).context("invalid cache key")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create cache dir")?;
        }
        let json = serde_json::to_vec_pretty(response).context("encode cache entry")?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).context("write cache temp")?;
        std::fs::rename(&tmp, &path).context("publish cache entry")?;
        Ok(())
    }
}

/// Home directory (`HOME` env, else the temp dir as a last resort).
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vlm::client::VlmResponse;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_dir() -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tag = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("razorpdf-cache-test-{}-{tag}", std::process::id()))
    }

    fn sample_response() -> VlmResponse {
        VlmResponse {
            markdown: "| a |\n| --- |\n| b |\n".to_string(),
            model: "mock-region".to_string(),
        }
    }

    #[test]
    fn roundtrip_put_get() {
        let dir = test_dir();
        let cache = Blake3VlmCache::new(&dir);
        let key = compute_vlm_cache_key(b"webpbytes", "m", "p");
        assert_eq!(key.len(), 64);
        assert!(cache.get(&key).is_none());
        cache.put(&key, &sample_response()).expect("put");
        let hit = cache.get(&key).expect("hit");
        assert_eq!(hit.markdown, sample_response().markdown);
        assert_eq!(hit.model, "mock-region");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_stable_and_prompt_sensitive() {
        let a = compute_vlm_cache_key(b"img", "m", "prompt-a");
        let b = compute_vlm_cache_key(b"img", "m", "prompt-a");
        let c = compute_vlm_cache_key(b"img", "m", "prompt-b");
        let d = compute_vlm_cache_key(b"img2", "m", "prompt-a");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn invalid_keys_rejected() {
        let cache = Blake3VlmCache::new(test_dir());
        assert!(cache.get("../../escape").is_none());
        assert!(cache.get("").is_none());
        assert!(cache.get("zz").is_none());
        assert!(cache.put("../../escape", &sample_response()).is_err());
    }

    #[test]
    fn missing_dir_is_miss_not_error() {
        let cache = Blake3VlmCache::new(test_dir().join("does-not-exist-xyz"));
        assert!(cache.get(&"a".repeat(64)).is_none());
    }

    #[test]
    fn corrupt_entry_is_miss() {
        let dir = test_dir();
        let cache = Blake3VlmCache::new(&dir);
        let key = "b".repeat(64);
        let path = dir.join("bb").join(format!("{key}.json"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{not json").expect("write");
        assert!(cache.get(&key).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
