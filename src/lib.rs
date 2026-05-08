//! Pure-Rust Cinepak (CVID) video decoder.
//!
//! Cinepak is a 1991 vector-quantisation video codec by SuperMac /
//! Radius / Providenza & Boekelheide. It operates on 4×4-pixel
//! macroblocks split across one or more horizontal **strips**, each
//! strip carrying its own pair of codebooks (V4 — four codebook entries
//! cover one 4×4 macroblock as 2×2 sub-blocks; V1 — one codebook entry
//! covers the whole 4×4 macroblock as four 2×2 quadrants of identical
//! luminance). Inter frames may additionally code "skip" macroblocks
//! that reuse the previous frame's reconstructed pixel block at the
//! same position (no motion vectors).
//!
//! This crate decodes both pixel-mode flavours:
//! - **12-bit YUV** — six-byte codebook entries `(Y0, Y1, Y2, Y3, U, V)`,
//!   with the inverse YUV→RGB matrix from the spec applied to produce
//!   packed [`CinepakPixelFormat::Rgb24`] output.
//! - **8-bit grayscale** — four-byte codebook entries `(Y0..Y3)`, no
//!   chroma; output is [`CinepakPixelFormat::Gray8`].
//!
//! ## Wire-format reference
//!
//! All decode behaviour is driven by `docs/video/cinepak/spec/*` in
//! the workspace clean-room workspace; see in particular:
//! - `01-frame-and-strip.md` — frame & strip headers.
//! - `02-codebooks.md` — codebook chunk taxonomy + V4/V1 entry layout.
//! - `03-vectors-and-macroblocks.md` — vector chunks `0x3000` /
//!   `0x3100` / `0x3200` and the V1 / V4 macroblock expansion rules.
//! - `04-yuv-rgb-matrix.md` — colour-space algebra (truncation toward
//!   zero on `U / 2`, clamp to `[0, 255]`).
//!
//! ## Standalone vs registry-integrated
//!
//! The default-on `registry` cargo feature pulls in `oxideav-core` and
//! installs the framework `Decoder` trait implementation plus
//! `register_codecs(reg)` / `register(ctx)` entry points. With the
//! feature off, the crate exposes a minimal `oxideav-core`-free API
//! built on `std`: `CinepakDecoder::new` + `decode_frame(&bytes)`
//! returning a crate-local [`CinepakFrame`].

#![forbid(unsafe_code)]

pub mod codebook;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod film;
pub mod header;
pub mod image;
pub mod vector;
pub mod yuv;

#[cfg(feature = "registry")]
pub mod registry;

/// Stable codec id used in the framework registry.
pub const CODEC_ID_STR: &str = "cinepak";

// Standalone, framework-free re-exports.
pub use decoder::CinepakDecoder;
pub use encoder::{
    encode_gray8, encode_rgb24, encode_rgb24_inter, CinepakEncoder, EncoderOptions, FrameStats,
    RateControlledFrame, TwoPassRateControl,
};
pub use error::{CinepakError, Result};
pub use film::{probe_film, Fdsc, FilmDemuxer, FilmHeader, SampleRecord, StabHeader};
pub use image::{CinepakFrame, CinepakPixelFormat, CinepakPlane};

// Framework-integrated re-exports.
#[cfg(feature = "registry")]
pub use registry::{__oxideav_entry, register, register_codecs};

#[cfg(all(test, feature = "registry"))]
mod register_tests {
    use oxideav_core::RuntimeContext;

    #[test]
    fn register_via_runtime_context_installs_factories() {
        let mut ctx = RuntimeContext::new();
        super::register(&mut ctx);
        assert!(
            ctx.codecs.decoder_ids().next().is_some(),
            "register(ctx) should install codec decoder factories"
        );
    }
}
