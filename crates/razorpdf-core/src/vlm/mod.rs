//! Sub-page WebP cropper & async VLM router.

pub mod client;
pub mod cropper;

pub use client::VlmClient;
pub use cropper::{crop_region_to_webp, region_to_pixels};
