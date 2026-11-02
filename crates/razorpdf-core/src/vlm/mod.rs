//! Sub-page WebP cropper & async VLM router.

pub mod cache;
pub mod client;
pub mod cropper;

pub use cache::{compute_vlm_cache_key, Blake3VlmCache};

pub use client::{
    strip_code_fences, HttpVlmProvider, MockVlmProvider, NoVlmProvider, VlmProvider, VlmRequest,
    VlmResponse, SYSTEM_PROMPT,
};
pub use cropper::{
    crop_region, crop_region_to_data_uri, crop_region_to_webp, encode_webp, region_to_pixels,
    render_synthetic_canvas,
};
