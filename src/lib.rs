// Per-MB raster loops are idiomatic; allow the lint.
#![allow(clippy::needless_range_loop)]
#![deny(missing_debug_implementations)]

//! Pure-Rust **Cinepak (CVID)** video decoder.
//!
//! Cinepak (Radius/Apple, 1991) is a vector-quantisation codec used in
//! mid-1990s QuickTime/AVI video. It has no DCT, no motion vectors,
//! no entropy coder — every multi-byte field is byte-aligned and big-
//! endian, and per-macroblock decoding is a 1- or 2-bit mode lookup
//! followed by 1-4 byte indices into a small per-strip codebook.
//!
//! See `docs/video/cinepak/cinepak-trace-reverse-engineering.md` in the
//! oxideav-workspace for the bitstream description we implemented from.
//!
//! ## Bitstream layers
//!
//! 1. **Frame header** (10 B): `frame_flags`, `encoded_buf_size`,
//!    width, height, num_strips.
//! 2. **Strip header** (12 B): `strip_id` (0x10 INTRA / 0x11 INTER),
//!    `strip_chunk_size`, rectangle (y1, x1, y2, x2). `strip_y1 == 0`
//!    means "stack onto running y cursor".
//! 3. **Chunks** within a strip:
//!    - `0x20 / 0x22` — V4 / V1 codebook full update, 6-byte entries.
//!    - `0x24 / 0x26` — V4 / V1 codebook full update, 4-byte entries
//!      (paletted, no chroma).
//!    - `0x21 / 0x23 / 0x25 / 0x27` — selective updates with 256
//!      bitmap-flag bits.
//!    - `0x30` — INTRA vector list, 1 mode bit per MB (V1 vs V4).
//!    - `0x31` — INTER vector list, 1 skip bit then optional 1 mode bit.
//!    - `0x32` — V1-only vector list, no flag bits.
//! 4. **MB-level** raster, 4x4 MBs, with bits pulled MSB-first from a
//!    32-bit big-endian flag register that reloads on demand.
//!
//! ## Output
//!
//! [`PixelFormat::Yuv420P`]. Each codebook entry's 2x2 luma quad and
//! signed `(u, v)` chroma pair are folded into the per-strip output
//! plane:
//! - V4 mode tiles four codebook entries into the 4x4 luma area, with
//!   each entry's (u, v) covering its 2x2 chroma cell — natural 4:2:0.
//! - V1 mode tiles one codebook entry's 2x2 luma quad into the 4x4
//!   luma area (each luma sample fills 2x2 pixels) and applies the
//!   single (u, v) to all 2x2 chroma cells of the MB — effectively
//!   4:1:1 across the 4x4 macroblock.
//!
//! Cinepak chroma is signed 8-bit and centred on zero; we map to the
//! standard YUV420P unsigned range with `Cb = u + 128`, `Cr = v + 128`.
//!
//! ## What's implemented
//!
//! - Frame + strip + chunk parsing (every chunk-id observed in the
//!   trace doc, plus the unobserved selective-update and 4-byte
//!   variants).
//! - V1 + V4 modes with full and selective codebook updates.
//! - INTRA (`0x30`) and INTER (`0x31`) vector lists.
//! - Skip MBs (copy from previous frame).
//! - Cross-strip codebook inheritance when `frame_flags & 1 == 0`.
//! - `strip_y1 == 0` running-y-cursor overload.
//! - Bottom-row fill-from-bottom for non-multiple-of-4 height.
//!
//! ## Gaps
//!
//! - 8-bit paletted output (`bits_per_coded_sample == 8`) is not
//!   implemented; we always render YUV420P with the unsigned-chroma
//!   mapping above.
//! - Sega FILM 2/6-byte filler probing is not implemented (no fixture).
//! - `0x32` V1-only vector lists are parsed (1 byte per MB, no flag bits).
//! - No encoder yet.

pub mod decoder;

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, CodecTag, Decoder,
    DecoderFactory, Result,
};

pub use decoder::CinepakDecoder;

/// Stable codec-id string (matches AVI FOURCC `cvid` lowercased).
pub const CODEC_ID_STR: &str = "cinepak";

/// Decoder factory — constructs a fresh [`CinepakDecoder`] honouring
/// the caller's [`CodecParameters`] (currently only `codec_id`; future
/// pixel-format hints would land here).
pub fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    Ok(Box::new(CinepakDecoder::new(params.codec_id.clone())))
}

/// Factory value for use in `CodecInfo::decoder(...)`.
pub const DECODER_FACTORY: DecoderFactory = make_decoder;

/// Short-hand `CodecId` constructor for `cinepak`.
pub fn cinepak_codec_id() -> CodecId {
    CodecId::new(CODEC_ID_STR)
}

/// Register the Cinepak decoder with a codec registry.
///
/// Two FOURCCs are claimed: AVI `cvid` and the uppercase `CVID` Apple
/// QuickTime variant. Real-world streams use both.
pub fn register(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("cinepak_sw")
        .with_lossy(true)
        .with_intra_only(false)
        .with_max_size(8192, 8192);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .tags([CodecTag::fourcc(b"cvid"), CodecTag::fourcc(b"CVID")]),
    );
}
