//! Standalone-friendly decoded-image and pixel-format types.
//!
//! Mirrors the pattern from `oxideav-mjpeg` so the crate compiles
//! without `oxideav-core` in the dep tree when the `registry` feature
//! is disabled. The `registry` module provides `From` impls that map
//! these types onto their `oxideav_core` counterparts.

/// Output pixel format produced by the Cinepak decoder.
///
/// Cinepak natively codes either 12-bit YUV (lossy chroma-subsampled at
/// 4×4 macroblock granularity) or 8-bit grayscale. The decoder applies
/// the YUV→RGB matrix (per `docs/video/cinepak/spec/04-yuv-rgb-matrix.md`)
/// and emits one of:
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CinepakPixelFormat {
    /// Packed 8-bit RGB, three bytes per pixel, in `R, G, B` order.
    /// Matches `pix_fmt rgb24`.
    Rgb24,
    /// Packed 8-bit grayscale, one byte per pixel.
    Gray8,
}

/// One plane of a [`CinepakFrame`].
#[derive(Clone, Debug)]
pub struct CinepakPlane {
    /// Bytes per row (no padding for the formats this crate emits).
    pub stride: usize,
    /// Plane pixel bytes.
    pub data: Vec<u8>,
}

/// A decoded Cinepak frame.
#[derive(Clone, Debug)]
pub struct CinepakFrame {
    pub width: u32,
    pub height: u32,
    pub pixel_format: CinepakPixelFormat,
    pub pts: Option<i64>,
    /// Always one plane: packed `Rgb24` or `Gray8`.
    pub planes: Vec<CinepakPlane>,
}

impl CinepakFrame {
    /// Convenience accessor for the (single) plane's pixel bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.planes[0].data
    }

    /// Convenience accessor for the (single) plane's stride.
    pub fn stride(&self) -> usize {
        self.planes[0].stride
    }
}
