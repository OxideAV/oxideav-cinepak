//! Cinepak frame-level encoder.
//!
//! Produces conformant Cinepak bitstreams (round 3: multi-strip, intra
//! and inter, mixed V1+V4 codebooks, optional skip macroblocks for
//! inter) that round-trip through this crate's decoder. The encoder is
//! **reference-grade**: it does not aim to match FFmpeg's per-byte
//! output or its rate-control behaviour, only to emit
//! syntactically-valid Cinepak frames whose decoded pixels recover the
//! input within codebook quantisation error.
//!
//! ## Algorithm overview
//!
//! Encoding proceeds per macroblock (4×4 RGB pixel block):
//!
//! 1. Convert the input RGB block to the codec's `(Y0..Y3, U, V)`
//!    representation using the **forward** of the spec's inverse
//!    matrix from `04-yuv-rgb-matrix.md`.
//! 2. Build candidate **V1** and **V4** codebooks via a median-cut
//!    quantiser over the per-macroblock vectors **of each strip**
//!    (one V1 vector per MB, four V4 sub-block vectors per MB).
//! 3. Choose **V1** vs **V4** per macroblock by lower mean-squared
//!    error against the original block, breaking ties toward V1
//!    (smaller wire footprint).
//! 4. For **inter** strips: when an MB matches the previous-frame
//!    reconstructed pixels within an MSE threshold, emit it as a
//!    SKIP code (0x3100 vector chunk grammar `0`). Otherwise emit
//!    the chosen V1/V4 update.
//! 5. Emit the strip with `0x2000` (V4 full) + `0x2200` (V1 full)
//!    codebook chunks (or `0x2400`/`0x2600` for grayscale) and a
//!    `0x3000` mixed-intra (or `0x3100` mixed-inter) vector chunk.
//!
//! Multi-strip frames split the image into N horizontal bands, each
//! coded independently with its own codebook pair. Strip count is
//! derived from `quality` (more strips = better local adaptation =
//! higher PSNR, at the cost of larger wire size from N codebook
//! chunks).
//!
//! ## Limitations
//!
//! - 12-bit YUV via `encode_rgb24` / `encode_rgb24_inter`; 8-bit
//!   grayscale via `encode_gray8`.
//! - No selective-update chunks for inter streams (decoder accepts
//!   them but our encoder uses full-replace per strip — simpler and
//!   matches FFmpeg's behaviour, spec §4.4).

// The encoder uses index-based loops (`for sub_idx in 0..4`) to keep
// the spatial-position arithmetic (`sub_row = idx / 2`, `sub_col = idx
// % 2`) inline with the index — switching to enumerate-based form
// hides that derivation behind a tuple.
#![allow(clippy::needless_range_loop)]

use crate::codebook::{
    encode_full_chunk, Codebook, CodebookChunkKind, CodebookEntry, PixelMode, UpdateStyle,
    WhichCodebook, CHUNK_HEADER_SIZE,
};
use crate::error::{CinepakError, Result};
use crate::header::{
    FrameHeader, RawStripHeader, FRAME_HEADER_SIZE, STRIP_HEADER_SIZE, STRIP_ID_INTER,
    STRIP_ID_INTRA,
};
use crate::image::{CinepakFrame, CinepakPixelFormat};
use crate::vector::{
    encode_inter_payload, encode_mixed_intra_payload, Mb, VECTOR_CHUNK_INTER, VECTOR_CHUNK_INTRA,
};

/// Encoder configuration.
///
/// Three knobs at most are touched directly:
///
/// - `v4_entries` / `v1_entries` — codebook size per strip
///   (1..=256 each).
/// - `strip_count` — number of horizontal strips per frame
///   (1..=number of macroblock rows).
/// - `skip_threshold` — MSE-per-pixel threshold below which an inter
///   macroblock is coded as SKIP. Lower = more updates = larger frame
///   but better quality; higher = more skips = smaller / lower quality.
///
/// For most users [`EncoderOptions::from_quality`] is sufficient: it
/// derives all three from a single `quality ∈ 0..=100` PSNR-style
/// knob (see method docs for the mapping).
#[derive(Clone, Copy, Debug)]
pub struct EncoderOptions {
    /// Number of V4 codebook entries (1..=256). Default `64`.
    pub v4_entries: u16,
    /// Number of V1 codebook entries (1..=256). Default `64`.
    pub v1_entries: u16,
    /// Number of horizontal strips per frame (1..=mb_rows). Default
    /// `1`. The encoder clamps this down to the number of macroblock
    /// rows when the frame is too short to host that many strips.
    pub strip_count: u16,
    /// Per-pixel MSE threshold below which an inter macroblock is
    /// coded as SKIP. Default `64.0` (≈ ±8 per channel).
    pub skip_threshold: f32,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        Self {
            v4_entries: 64,
            v1_entries: 64,
            strip_count: 1,
            skip_threshold: 64.0,
        }
    }
}

impl EncoderOptions {
    /// Map a `quality ∈ 0..=100` PSNR-style knob to a full options
    /// vector.
    ///
    /// Mapping (clamped at the endpoints):
    ///
    /// | quality | v4_entries | v1_entries | strip_count | skip_thr |
    /// | ------- | ---------- | ---------- | ----------- | -------- |
    /// |       0 |          8 |          8 |           1 |    256.0 |
    /// |      25 |         32 |         32 |           1 |    128.0 |
    /// |      50 |         64 |         64 |           2 |     64.0 |
    /// |      75 |        128 |        128 |           3 |     32.0 |
    /// |     100 |        256 |        256 |           4 |     16.0 |
    ///
    /// Higher quality ⇒ larger codebooks (more colour fidelity per
    /// strip), more strips (better local adaptation, especially for
    /// images with vertical colour gradients), and a stricter SKIP
    /// threshold (more updates emitted on inter frames, less ghosting).
    ///
    /// The mapping is intentionally coarse: Cinepak's quality is
    /// dominated by the 4×4-block structure and 12-bit YUV chroma
    /// quantisation, so doubling the codebook size only produces ~3 dB
    /// of PSNR gain on natural content.
    pub fn from_quality(quality: u8) -> Self {
        let q = quality.min(100) as f32;
        // Logarithmic-ish ramp on codebook size: 8..=256.
        // 2^(3 + 5*q/100) ⇒ q=0 → 8, q=100 → 256.
        let n = (2.0f32.powf(3.0 + 5.0 * q / 100.0)).round() as u16;
        let n = n.clamp(1, 256);
        // Strip count: 1..=4 across the q range, with thresholds at
        // q=33 / 66 / 100. We don't go higher than 4 because each
        // extra strip costs a full codebook chunk.
        let strip_count = if q < 33.0 {
            1
        } else if q < 66.0 {
            2
        } else if q < 100.0 {
            3
        } else {
            4
        };
        // Skip threshold: 256.0 at q=0 → 16.0 at q=100, exponential.
        let skip_threshold = 256.0 * 2.0f32.powf(-q / 25.0);
        Self {
            v4_entries: n,
            v1_entries: n,
            strip_count,
            skip_threshold,
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Encode a 12-bit YUV intra frame from packed `Rgb24` input
/// (`width × height × 3` bytes, R, G, B in row-major scan).
pub fn encode_rgb24(rgb: &[u8], width: u32, height: u32, opts: EncoderOptions) -> Result<Vec<u8>> {
    encode_intra_frame(rgb, width, height, PixelMode::Yuv12, opts)
}

/// Encode an 8-bit grayscale intra frame from packed `Gray8` input
/// (`width × height` bytes).
pub fn encode_gray8(gray: &[u8], width: u32, height: u32, opts: EncoderOptions) -> Result<Vec<u8>> {
    encode_intra_frame(gray, width, height, PixelMode::Gray8, opts)
}

/// Encode a 12-bit YUV inter frame from packed `Rgb24` input,
/// referencing `prev` for SKIP-MB selection.
///
/// `prev` must be an `Rgb24` reconstructed frame of the same dimensions
/// as `(width, height)` — typically the previous frame this encoder
/// emitted, decoded back through [`crate::CinepakDecoder`].
///
/// The encoder emits a single `0x1100` strip (or several, per
/// `opts.strip_count`) carrying full-replace codebook chunks and a
/// `0x3100` mixed-inter vector chunk. Macroblocks whose per-pixel MSE
/// against the same-position `prev` block is below
/// `opts.skip_threshold` are coded as SKIP.
pub fn encode_rgb24_inter(
    rgb: &[u8],
    prev: &CinepakFrame,
    width: u32,
    height: u32,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    if prev.width != width || prev.height != height {
        return Err(CinepakError::other(format!(
            "encoder: prev frame dims {}x{} != current {width}x{height}",
            prev.width, prev.height
        )));
    }
    if prev.pixel_format != CinepakPixelFormat::Rgb24 {
        return Err(CinepakError::other(
            "encoder: encode_rgb24_inter requires Rgb24 prev frame",
        ));
    }
    encode_inter_frame(rgb, prev, width, height, PixelMode::Yuv12, opts)
}

// ---------------------------------------------------------------------------
// Intra encode
// ---------------------------------------------------------------------------

fn encode_intra_frame(
    pixels: &[u8],
    width: u32,
    height: u32,
    mode: PixelMode,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    validate_dims(width, height)?;
    validate_opts(&opts)?;
    validate_input_size(pixels, width, height, mode)?;

    let mb_rows = (height / 4) as usize;
    let strips = plan_strips(mb_rows, opts.strip_count as usize);

    let mut frame_strips: Vec<Vec<u8>> = Vec::with_capacity(strips.len());
    for s in &strips {
        let bytes = encode_intra_strip(pixels, width, mode, &opts, s)?;
        frame_strips.push(bytes);
    }
    assemble_frame(width, height, &frame_strips)
}

// ---------------------------------------------------------------------------
// Inter encode
// ---------------------------------------------------------------------------

fn encode_inter_frame(
    pixels: &[u8],
    prev: &CinepakFrame,
    width: u32,
    height: u32,
    mode: PixelMode,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    validate_dims(width, height)?;
    validate_opts(&opts)?;
    validate_input_size(pixels, width, height, mode)?;

    let mb_rows = (height / 4) as usize;
    let strips = plan_strips(mb_rows, opts.strip_count as usize);

    let mut frame_strips: Vec<Vec<u8>> = Vec::with_capacity(strips.len());
    for s in &strips {
        let bytes = encode_inter_strip(pixels, prev, width, mode, &opts, s)?;
        frame_strips.push(bytes);
    }
    assemble_frame(width, height, &frame_strips)
}

// ---------------------------------------------------------------------------
// Per-strip encoders
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct StripPlan {
    /// Pixel-coordinate top of the strip (inclusive).
    y_top: u32,
    /// Pixel-coordinate bottom of the strip (exclusive).
    y_bottom: u32,
}

fn plan_strips(mb_rows: usize, requested: usize) -> Vec<StripPlan> {
    let n = requested.clamp(1, mb_rows.max(1));
    // Distribute MB rows as evenly as possible; remainder goes to
    // the first `mb_rows % n` strips.
    let base = mb_rows / n;
    let rem = mb_rows % n;
    let mut out = Vec::with_capacity(n);
    let mut y_mb = 0usize;
    for i in 0..n {
        let h_mb = base + if i < rem { 1 } else { 0 };
        let y_top = (y_mb * 4) as u32;
        let y_bot = ((y_mb + h_mb) * 4) as u32;
        out.push(StripPlan {
            y_top,
            y_bottom: y_bot,
        });
        y_mb += h_mb;
    }
    out
}

fn encode_intra_strip(
    pixels: &[u8],
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
) -> Result<Vec<u8>> {
    let plan = build_codebooks_and_decisions(pixels, width, mode, opts, s, None, false)?;
    let StripPlanResult { v4_cb, v1_cb, mbs } = plan;

    let mut chunks = Vec::new();
    emit_codebook_chunks(mode, &v4_cb, &v1_cb, opts, &mut chunks);

    // Vector chunk 0x3000 (mixed intra).
    let mut vec_payload = Vec::new();
    encode_mixed_intra_payload(&mbs, &mut vec_payload)?;
    let vec_chunk_size = (CHUNK_HEADER_SIZE + vec_payload.len()) as u16;
    chunks.extend_from_slice(&VECTOR_CHUNK_INTRA.to_be_bytes());
    chunks.extend_from_slice(&vec_chunk_size.to_be_bytes());
    chunks.extend_from_slice(&vec_payload);

    finalise_strip(STRIP_ID_INTRA, s, width, &chunks)
}

fn encode_inter_strip(
    pixels: &[u8],
    prev: &CinepakFrame,
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
) -> Result<Vec<u8>> {
    let plan = build_codebooks_and_decisions(pixels, width, mode, opts, s, Some(prev), true)?;
    let StripPlanResult { v4_cb, v1_cb, mbs } = plan;

    let mut chunks = Vec::new();
    emit_codebook_chunks(mode, &v4_cb, &v1_cb, opts, &mut chunks);

    // Vector chunk 0x3100 (mixed inter with skip).
    let mut vec_payload = Vec::new();
    encode_inter_payload(&mbs, &mut vec_payload)?;
    let vec_chunk_size = (CHUNK_HEADER_SIZE + vec_payload.len()) as u16;
    chunks.extend_from_slice(&VECTOR_CHUNK_INTER.to_be_bytes());
    chunks.extend_from_slice(&vec_chunk_size.to_be_bytes());
    chunks.extend_from_slice(&vec_payload);

    finalise_strip(STRIP_ID_INTER, s, width, &chunks)
}

/// Per-strip codebook + per-MB decision result.
struct StripPlanResult {
    v4_cb: Codebook,
    v1_cb: Codebook,
    mbs: Vec<Mb>,
}

/// Sample the strip's macroblocks, build V1 + V4 codebooks via
/// median-cut, and decide V1 / V4 / Skip per MB.
///
/// On `is_inter == true` and `prev = Some(...)`, MBs whose per-pixel
/// MSE against the prev frame is below `opts.skip_threshold` are coded
/// as `Mb::Skip`.
fn build_codebooks_and_decisions(
    pixels: &[u8],
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
    prev: Option<&CinepakFrame>,
    is_inter: bool,
) -> Result<StripPlanResult> {
    let mb_cols = (width / 4) as usize;
    let strip_h_mb = ((s.y_bottom - s.y_top) / 4) as usize;
    let mb_count = strip_h_mb * mb_cols;

    let mut v4_vectors: Vec<CodebookEntry> = Vec::with_capacity(mb_count * 4);
    let mut v1_vectors: Vec<CodebookEntry> = Vec::with_capacity(mb_count);
    let mut mb_v4: Vec<[CodebookEntry; 4]> = Vec::with_capacity(mb_count);
    let mut mb_v1: Vec<CodebookEntry> = Vec::with_capacity(mb_count);
    // Track which MBs are SKIP candidates so we don't pollute the
    // codebook training set with their data (they don't need codebook
    // entries).
    let mut skip_mask: Vec<bool> = vec![false; mb_count];

    for r in 0..strip_h_mb {
        for c in 0..mb_cols {
            let py = s.y_top as usize + r * 4;
            let px = c * 4;
            let v4 = sample_v4_block(pixels, width as usize, px, py, mode);
            let v1 = sample_v1_block(pixels, width as usize, px, py, mode);

            let is_skip = if is_inter {
                if let Some(prev_f) = prev {
                    let mse = mb_mse_against_prev(pixels, prev_f, px, py, width, mode);
                    mse < opts.skip_threshold
                } else {
                    false
                }
            } else {
                false
            };

            let mb_idx = r * mb_cols + c;
            skip_mask[mb_idx] = is_skip;

            if !is_skip {
                v4_vectors.extend_from_slice(&v4);
                v1_vectors.push(v1);
            }
            mb_v4.push(v4);
            mb_v1.push(v1);
        }
    }

    // Build codebooks via median-cut on the non-skipped vectors. If
    // every MB is a skip, leave codebooks at default (zero) — fine,
    // they won't be referenced.
    let v4_codebook = median_cut(&v4_vectors, opts.v4_entries as usize, mode);
    let v1_codebook = median_cut(&v1_vectors, opts.v1_entries as usize, mode);

    // Classify each MB.
    let mut mbs: Vec<Mb> = Vec::with_capacity(mb_count);
    for i in 0..mb_count {
        if skip_mask[i] {
            mbs.push(Mb::Skip);
            continue;
        }
        let (v4_idx, v4_err) = pick_v4(&mb_v4[i], &v4_codebook, opts.v4_entries as usize);
        let (v1_idx, v1_err) = pick_v1(&mb_v1[i], &v1_codebook, opts.v1_entries as usize, mode);
        // Tiebreak toward V1 (smaller wire footprint).
        if v1_err <= v4_err {
            mbs.push(Mb::V1(v1_idx));
        } else {
            mbs.push(Mb::V4(v4_idx));
        }
    }

    Ok(StripPlanResult {
        v4_cb: v4_codebook,
        v1_cb: v1_codebook,
        mbs,
    })
}

fn emit_codebook_chunks(
    mode: PixelMode,
    v4_cb: &Codebook,
    v1_cb: &Codebook,
    opts: &EncoderOptions,
    out: &mut Vec<u8>,
) {
    let v4_kind = CodebookChunkKind {
        which: WhichCodebook::V4,
        style: UpdateStyle::Full,
        mode,
    };
    let v1_kind = CodebookChunkKind {
        which: WhichCodebook::V1,
        style: UpdateStyle::Full,
        mode,
    };
    encode_full_chunk(v4_kind, v4_cb, opts.v4_entries as usize, out);
    encode_full_chunk(v1_kind, v1_cb, opts.v1_entries as usize, out);
}

fn finalise_strip(strip_id: u16, s: &StripPlan, width: u32, chunks: &[u8]) -> Result<Vec<u8>> {
    let strip_size_usize = STRIP_HEADER_SIZE + chunks.len();
    if strip_size_usize > u16::MAX as usize {
        return Err(CinepakError::other(format!(
            "encoder: strip exceeds 16-bit strip_size budget ({strip_size_usize} > 65535); reduce codebook size or split into more strips"
        )));
    }
    let strip_size = strip_size_usize as u16;
    let raw = RawStripHeader {
        strip_id,
        strip_size,
        y_top: s.y_top as u16,
        x_top: 0,
        y_bottom: s.y_bottom as u16,
        x_bottom: width as u16,
    };
    let mut out = Vec::with_capacity(strip_size as usize);
    let mut hdr_buf = [0u8; STRIP_HEADER_SIZE];
    raw.encode(&mut hdr_buf);
    out.extend_from_slice(&hdr_buf);
    out.extend_from_slice(chunks);
    Ok(out)
}

fn assemble_frame(width: u32, height: u32, strips: &[Vec<u8>]) -> Result<Vec<u8>> {
    let strips_total: usize = strips.iter().map(|s| s.len()).sum();
    let frame_length_usize = FRAME_HEADER_SIZE + strips_total;
    if frame_length_usize > 0x00ff_ffff {
        return Err(CinepakError::other(format!(
            "encoder: frame_length exceeds 24-bit budget ({frame_length_usize} > 16777215)"
        )));
    }
    let frame_length = frame_length_usize as u32;
    let frame_hdr = FrameHeader {
        flags: 0,
        frame_length,
        width: width as u16,
        height: height as u16,
        strip_count: strips.len() as u16,
    };
    let mut out = Vec::with_capacity(frame_length as usize);
    let mut fhdr_buf = [0u8; FRAME_HEADER_SIZE];
    frame_hdr.encode(&mut fhdr_buf);
    out.extend_from_slice(&fhdr_buf);
    for s in strips {
        out.extend_from_slice(s);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_dims(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || width % 4 != 0 || height % 4 != 0 {
        return Err(CinepakError::other(format!(
            "encoder: dims must be > 0 and multiples of 4; got {width}x{height}"
        )));
    }
    Ok(())
}

fn validate_opts(opts: &EncoderOptions) -> Result<()> {
    if !(1..=256).contains(&(opts.v4_entries as u32))
        || !(1..=256).contains(&(opts.v1_entries as u32))
    {
        return Err(CinepakError::other(
            "encoder: v4_entries / v1_entries must be in 1..=256",
        ));
    }
    if opts.strip_count == 0 {
        return Err(CinepakError::other("encoder: strip_count must be ≥ 1"));
    }
    if !(opts.skip_threshold.is_finite() && opts.skip_threshold >= 0.0) {
        return Err(CinepakError::other(
            "encoder: skip_threshold must be a finite non-negative float",
        ));
    }
    Ok(())
}

fn validate_input_size(pixels: &[u8], width: u32, height: u32, mode: PixelMode) -> Result<()> {
    let bpp = match mode {
        PixelMode::Yuv12 => 3,
        PixelMode::Gray8 => 1,
    };
    let expected = (width as usize) * (height as usize) * bpp;
    if pixels.len() < expected {
        return Err(CinepakError::other(format!(
            "encoder: input buffer {} bytes < expected {expected}",
            pixels.len()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// RGB → YUV sampling
// ---------------------------------------------------------------------------

/// Forward of the spec's inverse matrix (yuv → rgb in
/// `04-yuv-rgb-matrix.md` §3):
///
/// ```text
///   R = Y + 2V        (1)
///   G = Y - U/2 - V   (2)
///   B = Y + 2U        (3)
/// ```
///
/// Solve (1)..(3) simultaneously for `(Y, U, V)`:
///
/// ```text
///   V = (R - Y) / 2          from (1)
///   U = (B - Y) / 2          from (3)
///   substitute in (2):
///     G = Y - (B - Y)/4 - (R - Y)/2
///       = Y - B/4 + Y/4 - R/2 + Y/2
///       = (7/4) Y - B/4 - R/2
///   ⇒  Y = (4G + B + 2R) / 7
/// ```
///
/// This formula round-trips the primary fixtures `T1a..T1e` exactly
/// (per spec/02 §6.1 table): pure red `(255, 0, 0)` gives `(73, -36,
/// 91)` (decoder: `Y=73, U=-36, V=91 → R=255, G=0, B=1`); pure green
/// `(0, 255, 0)` gives `(145, -73, -73)`, decoder `(0, 254, 0)` —
/// matching `T1c`'s spec-quoted `M2` fixture. Pure blue `(0, 0, 255)`
/// gives `(36, +110, -18)` ≈ wire fixture `T1b`'s `(108, -19)`.
#[inline]
fn rgb_to_yuv(r: u8, g: u8, b: u8) -> (u8, i8, i8) {
    let r = r as i32;
    let g = g as i32;
    let b = b as i32;
    // Y = (4G + B + 2R) / 7, with rounding-to-nearest.
    let y = (2 * r + 4 * g + b + 3) / 7;
    let y = y.clamp(0, 255);
    let u = (b - y) / 2;
    let v = (r - y) / 2;
    (y as u8, u.clamp(-128, 127) as i8, v.clamp(-128, 127) as i8)
}

/// Sample one V4 macroblock's four 2×2 sub-blocks. Returns four
/// codebook-entry values, one per sub-block (TL, TR, BL, BR).
fn sample_v4_block(
    pixels: &[u8],
    pixel_stride_pixels: usize,
    px: usize,
    py: usize,
    mode: PixelMode,
) -> [CodebookEntry; 4] {
    let mut out = [CodebookEntry::default(); 4];
    for sub_idx in 0..4 {
        let sub_row = sub_idx / 2;
        let sub_col = sub_idx % 2;
        // Each entry's Yi is one of the four pixels in the 2×2 sub-block.
        // Layout: Y0 at (0,0), Y1 at (0,1), Y2 at (1,0), Y3 at (1,1).
        let mut ys = [0u8; 4];
        let (mut acc_r, mut acc_g, mut acc_b) = (0i32, 0i32, 0i32);
        for pixel_idx in 0..4 {
            let dy = pixel_idx / 2;
            let dx = pixel_idx % 2;
            let row = py + sub_row * 2 + dy;
            let col = px + sub_col * 2 + dx;
            match mode {
                PixelMode::Yuv12 => {
                    let off = row * pixel_stride_pixels * 3 + col * 3;
                    let (r, g, b) = (pixels[off], pixels[off + 1], pixels[off + 2]);
                    let (y, _, _) = rgb_to_yuv(r, g, b);
                    ys[pixel_idx] = y;
                    acc_r += i32::from(r);
                    acc_g += i32::from(g);
                    acc_b += i32::from(b);
                }
                PixelMode::Gray8 => {
                    let off = row * pixel_stride_pixels + col;
                    ys[pixel_idx] = pixels[off];
                }
            }
        }
        let (u, v) = match mode {
            PixelMode::Yuv12 => {
                // Average chroma across the 2×2 sub-block.
                let r = (acc_r / 4) as u8;
                let g = (acc_g / 4) as u8;
                let b = (acc_b / 4) as u8;
                let (_, u, v) = rgb_to_yuv(r, g, b);
                (u, v)
            }
            PixelMode::Gray8 => (0, 0),
        };
        out[sub_idx] = CodebookEntry {
            y0: ys[0],
            y1: ys[1],
            y2: ys[2],
            y3: ys[3],
            u,
            v,
        };
    }
    out
}

/// Sample one V1 macroblock as a single codebook entry. Each `Yi`
/// represents the average luminance over one 2×2 quadrant of the 4×4
/// macroblock.
fn sample_v1_block(
    pixels: &[u8],
    pixel_stride_pixels: usize,
    px: usize,
    py: usize,
    mode: PixelMode,
) -> CodebookEntry {
    let mut quad_ys = [0u8; 4];
    let (mut acc_r, mut acc_g, mut acc_b) = (0i32, 0i32, 0i32);
    let mut total_pixels = 0i32;
    for quad_idx in 0..4 {
        let quad_row = quad_idx / 2;
        let quad_col = quad_idx % 2;
        let mut sum_y = 0i32;
        for dy in 0..2 {
            for dx in 0..2 {
                let row = py + quad_row * 2 + dy;
                let col = px + quad_col * 2 + dx;
                match mode {
                    PixelMode::Yuv12 => {
                        let off = row * pixel_stride_pixels * 3 + col * 3;
                        let (r, g, b) = (pixels[off], pixels[off + 1], pixels[off + 2]);
                        let (y, _, _) = rgb_to_yuv(r, g, b);
                        sum_y += i32::from(y);
                        acc_r += i32::from(r);
                        acc_g += i32::from(g);
                        acc_b += i32::from(b);
                        total_pixels += 1;
                    }
                    PixelMode::Gray8 => {
                        let off = row * pixel_stride_pixels + col;
                        sum_y += i32::from(pixels[off]);
                        total_pixels += 1;
                    }
                }
            }
        }
        quad_ys[quad_idx] = (sum_y / 4) as u8;
    }
    let (u, v) = match mode {
        PixelMode::Yuv12 => {
            let r = (acc_r / total_pixels) as u8;
            let g = (acc_g / total_pixels) as u8;
            let b = (acc_b / total_pixels) as u8;
            let (_, u, v) = rgb_to_yuv(r, g, b);
            (u, v)
        }
        PixelMode::Gray8 => (0, 0),
    };
    CodebookEntry {
        y0: quad_ys[0],
        y1: quad_ys[1],
        y2: quad_ys[2],
        y3: quad_ys[3],
        u,
        v,
    }
}

// ---------------------------------------------------------------------------
// Inter-frame skip detection
// ---------------------------------------------------------------------------

/// Per-pixel mean-squared error between the source 4×4 block at
/// `(px, py)` in the input RGB buffer and the same-position 4×4 block
/// in `prev`. Used to decide SKIP eligibility for inter macroblocks.
///
/// For `Yuv12` mode, error is computed over the three RGB channels —
/// not the encoded YUV space — because the SKIP decision is about the
/// **output** pixel buffer that will be visible to the decoder. For
/// `Gray8`, error is over the single luminance channel.
fn mb_mse_against_prev(
    pixels: &[u8],
    prev: &CinepakFrame,
    px: usize,
    py: usize,
    width: u32,
    mode: PixelMode,
) -> f32 {
    let prev_stride = prev.planes[0].stride;
    let prev_data = &prev.planes[0].data;
    let mut sum_sq: f32 = 0.0;
    let mut n: f32 = 0.0;
    match mode {
        PixelMode::Yuv12 => {
            for dy in 0..4 {
                for dx in 0..4 {
                    let row = py + dy;
                    let col = px + dx;
                    let src_off = row * (width as usize) * 3 + col * 3;
                    let prev_off = row * prev_stride + col * 3;
                    for ch in 0..3 {
                        let s = pixels[src_off + ch] as f32;
                        let p = prev_data[prev_off + ch] as f32;
                        let d = s - p;
                        sum_sq += d * d;
                        n += 1.0;
                    }
                }
            }
        }
        PixelMode::Gray8 => {
            for dy in 0..4 {
                for dx in 0..4 {
                    let row = py + dy;
                    let col = px + dx;
                    let src_off = row * (width as usize) + col;
                    let prev_off = row * prev_stride + col;
                    let s = pixels[src_off] as f32;
                    let p = prev_data[prev_off] as f32;
                    let d = s - p;
                    sum_sq += d * d;
                    n += 1.0;
                }
            }
        }
    }
    if n > 0.0 {
        sum_sq / n
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Median-cut codebook quantiser
// ---------------------------------------------------------------------------

/// Build a codebook of up to `n` entries from the population
/// `vectors`. Median-cut: recursively bisect the population along the
/// dimension of greatest range, each cut producing two sub-clusters.
/// Each leaf cluster contributes one codebook entry — the centroid of
/// its members.
fn median_cut(vectors: &[CodebookEntry], n: usize, mode: PixelMode) -> Codebook {
    let mut cb = Codebook::default();
    if vectors.is_empty() || n == 0 {
        return cb;
    }
    // Build an initial single cluster.
    let dims = match mode {
        PixelMode::Yuv12 => 6,
        PixelMode::Gray8 => 4,
    };
    let mut clusters: Vec<Vec<CodebookEntry>> = vec![vectors.to_vec()];
    while clusters.len() < n {
        // Find cluster with largest extent along any dimension.
        let mut best_idx = None;
        let mut best_extent = 0i32;
        let mut best_dim = 0;
        for (ci, c) in clusters.iter().enumerate() {
            if c.len() < 2 {
                continue;
            }
            for d in 0..dims {
                let (lo, hi) = extent(c, d);
                let ext = hi - lo;
                if ext > best_extent {
                    best_extent = ext;
                    best_idx = Some(ci);
                    best_dim = d;
                }
            }
        }
        let Some(idx) = best_idx else {
            break;
        };
        if best_extent == 0 {
            break;
        }
        let mut cluster = std::mem::take(&mut clusters[idx]);
        cluster.sort_by_key(|e| dim_value(e, best_dim));
        let mid = cluster.len() / 2;
        let right = cluster.split_off(mid);
        clusters[idx] = cluster;
        clusters.push(right);
    }
    // Compute centroids and write to codebook entries.
    for (i, c) in clusters.iter().enumerate().take(n) {
        if c.is_empty() {
            continue;
        }
        cb.entries[i] = centroid(c, mode);
    }
    cb
}

fn dim_value(e: &CodebookEntry, d: usize) -> i32 {
    match d {
        0 => i32::from(e.y0),
        1 => i32::from(e.y1),
        2 => i32::from(e.y2),
        3 => i32::from(e.y3),
        4 => i32::from(e.u),
        5 => i32::from(e.v),
        _ => 0,
    }
}

fn extent(c: &[CodebookEntry], d: usize) -> (i32, i32) {
    let mut lo = i32::MAX;
    let mut hi = i32::MIN;
    for e in c {
        let v = dim_value(e, d);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (lo, hi)
}

fn centroid(c: &[CodebookEntry], mode: PixelMode) -> CodebookEntry {
    let n = c.len() as i32;
    let mut s = [0i64; 6];
    for e in c {
        s[0] += i64::from(e.y0);
        s[1] += i64::from(e.y1);
        s[2] += i64::from(e.y2);
        s[3] += i64::from(e.y3);
        s[4] += i64::from(e.u);
        s[5] += i64::from(e.v);
    }
    let div = i64::from(n);
    let y0 = (s[0] / div) as u8;
    let y1 = (s[1] / div) as u8;
    let y2 = (s[2] / div) as u8;
    let y3 = (s[3] / div) as u8;
    let (u, v) = match mode {
        PixelMode::Yuv12 => ((s[4] / div) as i8, (s[5] / div) as i8),
        PixelMode::Gray8 => (0, 0),
    };
    CodebookEntry {
        y0,
        y1,
        y2,
        y3,
        u,
        v,
    }
}

// ---------------------------------------------------------------------------
// Per-MB nearest-neighbour selection
// ---------------------------------------------------------------------------

fn entry_distance(a: &CodebookEntry, b: &CodebookEntry, mode: PixelMode) -> i64 {
    let dy0 = i64::from(a.y0) - i64::from(b.y0);
    let dy1 = i64::from(a.y1) - i64::from(b.y1);
    let dy2 = i64::from(a.y2) - i64::from(b.y2);
    let dy3 = i64::from(a.y3) - i64::from(b.y3);
    let mut d = dy0 * dy0 + dy1 * dy1 + dy2 * dy2 + dy3 * dy3;
    if let PixelMode::Yuv12 = mode {
        let du = i64::from(a.u) - i64::from(b.u);
        let dv = i64::from(a.v) - i64::from(b.v);
        d += du * du + dv * dv;
    }
    d
}

fn nearest(target: &CodebookEntry, cb: &Codebook, n: usize, mode: PixelMode) -> (u8, i64) {
    let mut best_idx = 0u8;
    let mut best_err = i64::MAX;
    for i in 0..n.min(256) {
        let d = entry_distance(target, &cb.entries[i], mode);
        if d < best_err {
            best_err = d;
            best_idx = i as u8;
        }
    }
    (best_idx, best_err)
}

fn pick_v4(target: &[CodebookEntry; 4], cb: &Codebook, n: usize) -> ([u8; 4], i64) {
    let mut idx = [0u8; 4];
    let mut err = 0i64;
    for sub in 0..4 {
        let (i, e) = nearest(&target[sub], cb, n, PixelMode::Yuv12);
        idx[sub] = i;
        err += e;
    }
    (idx, err)
}

fn pick_v1(target: &CodebookEntry, cb: &Codebook, n: usize, mode: PixelMode) -> (u8, i64) {
    nearest(target, cb, n, mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CinepakDecoder;

    /// Round-trip: encode a synthesised RGB frame, decode it, and
    /// confirm decoded pixels are within a small per-channel tolerance.
    /// The tolerance accounts for codebook quantisation error; with 64
    /// entries and a 16×16 frame of 4 distinct colour blocks, the
    /// quantiser should reproduce each block exactly.
    #[test]
    fn rgb24_roundtrip_4_color_blocks() {
        // 16×16 frame: four 8×8 quadrants, each one solid colour.
        let w = 16usize;
        let h = 16usize;
        let mut rgb = vec![0u8; w * h * 3];
        let colors = [
            (255, 0, 0),   // red TL
            (0, 255, 0),   // green TR
            (0, 0, 255),   // blue BL
            (200, 200, 0), // yellow BR
        ];
        for r in 0..h {
            for c in 0..w {
                let q = (r / 8) * 2 + (c / 8); // 0..3
                let off = r * w * 3 + c * 3;
                rgb[off] = colors[q].0;
                rgb[off + 1] = colors[q].1;
                rgb[off + 2] = colors[q].2;
            }
        }
        let bytes = encode_rgb24(&rgb, w as u32, h as u32, EncoderOptions::default()).unwrap();
        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.width, w as u32);
        assert_eq!(f.height, h as u32);
        let s = f.stride();
        let p = f.pixels();
        // Sample one pixel in each quadrant; codebook should reproduce
        // the dominant block colour within YUV-quantisation tolerance.
        for q in 0..4 {
            let qr = (q / 2) * 8 + 2;
            let qc = (q % 2) * 8 + 2;
            let off = qr * s + qc * 3;
            let r = p[off] as i32;
            let g = p[off + 1] as i32;
            let b = p[off + 2] as i32;
            let (er, eg, eb) = (colors[q].0 as i32, colors[q].1 as i32, colors[q].2 as i32);
            // The YUV inverse of (255, 0, 0) etc. has up to ~1 unit
            // of round-off in Y; allow ±8 per channel.
            assert!((r - er).abs() <= 8, "q{q} R={r} expect {er}");
            assert!((g - eg).abs() <= 8, "q{q} G={g} expect {eg}");
            assert!((b - eb).abs() <= 8, "q{q} B={b} expect {eb}");
        }
    }

    /// Round-trip: 8-bit grayscale.
    #[test]
    fn gray8_roundtrip_4_blocks() {
        let w = 16usize;
        let h = 16usize;
        let mut gray = vec![0u8; w * h];
        let lums = [40u8, 80, 160, 220];
        for r in 0..h {
            for c in 0..w {
                let q = (r / 8) * 2 + (c / 8);
                gray[r * w + c] = lums[q];
            }
        }
        let bytes = encode_gray8(&gray, w as u32, h as u32, EncoderOptions::default()).unwrap();
        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.pixel_format, crate::CinepakPixelFormat::Gray8);
        assert_eq!(f.stride(), w);
        let p = f.pixels();
        for q in 0..4 {
            let qr = (q / 2) * 8 + 2;
            let qc = (q % 2) * 8 + 2;
            let v = p[qr * w + qc];
            assert!(
                (v as i32 - lums[q] as i32).abs() <= 4,
                "q{q} got {v} expect {}",
                lums[q]
            );
        }
    }

    /// Encoder rejects non-multiple-of-4 dims (matches header parser).
    #[test]
    fn rejects_misaligned_dims() {
        let rgb = vec![0u8; 5 * 4 * 3];
        let r = encode_rgb24(&rgb, 5, 4, EncoderOptions::default());
        assert!(r.is_err());
    }

    /// Encoder produces a stream whose declared frame_length matches
    /// its byte length exactly.
    #[test]
    fn frame_length_matches_buffer_size() {
        let rgb = vec![128u8; 8 * 8 * 3];
        let bytes = encode_rgb24(&rgb, 8, 8, EncoderOptions::default()).unwrap();
        let h = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(h.frame_length as usize, bytes.len());
        assert_eq!(h.width, 8);
        assert_eq!(h.height, 8);
        assert_eq!(h.strip_count, 1);
    }

    /// Multi-strip planning: 8 MB rows split into 3 strips ⇒ 3+3+2.
    #[test]
    fn plan_strips_distributes_remainder_to_first() {
        let plans = plan_strips(8, 3);
        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0].y_top, 0);
        assert_eq!(plans[0].y_bottom, 12); // 3 MB rows × 4 px
        assert_eq!(plans[1].y_top, 12);
        assert_eq!(plans[1].y_bottom, 24); // 3 MB rows
        assert_eq!(plans[2].y_top, 24);
        assert_eq!(plans[2].y_bottom, 32); // 2 MB rows
    }

    /// Multi-strip planning: requested strips clamped to MB-row count.
    #[test]
    fn plan_strips_clamps_to_mb_rows() {
        let plans = plan_strips(2, 8);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].y_bottom, 4);
        assert_eq!(plans[1].y_bottom, 8);
    }

    /// Multi-strip encode: a 32×32 frame with `strip_count = 4` should
    /// produce 4 strips that round-trip through the decoder.
    #[test]
    fn encode_multi_strip_4_strips_32x32() {
        let w = 32usize;
        let h = 32usize;
        let mut rgb = vec![0u8; w * h * 3];
        // Vertical gradient: each strip should learn its own band.
        for r in 0..h {
            let g = (r * 8) as u8;
            for c in 0..w {
                let off = r * w * 3 + c * 3;
                rgb[off] = g;
                rgb[off + 1] = g;
                rgb[off + 2] = g;
            }
        }
        let opts = EncoderOptions {
            v4_entries: 8,
            v1_entries: 8,
            strip_count: 4,
            skip_threshold: 64.0,
        };
        let bytes = encode_rgb24(&rgb, w as u32, h as u32, opts).unwrap();
        let h_parsed = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(h_parsed.strip_count, 4);
        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.width, 32);
        assert_eq!(f.height, 32);
        // Spot check: row 4 should be ~32, row 28 should be ~224.
        let p = f.pixels();
        let s = f.stride();
        let r4 = p[4 * s + 8] as i32;
        let r28 = p[28 * s + 8] as i32;
        assert!(r4 < 80, "row 4 luma {r4} should be dark");
        assert!(r28 > 160, "row 28 luma {r28} should be bright");
    }

    /// Inter encode: an unchanged frame should produce an inter frame
    /// where most macroblocks are SKIP.
    #[test]
    fn encode_inter_unchanged_frame_is_mostly_skip() {
        let w = 16usize;
        let h = 16usize;
        // Solid mid-gray — uniform.
        let rgb = vec![128u8; w * h * 3];
        let bytes_intra =
            encode_rgb24(&rgb, w as u32, h as u32, EncoderOptions::default()).unwrap();
        let mut dec = CinepakDecoder::new();
        let prev = dec.decode_frame(&bytes_intra, None).unwrap();

        // Inter encode the same frame again, against `prev`.
        let opts = EncoderOptions {
            v4_entries: 8,
            v1_entries: 8,
            strip_count: 1,
            skip_threshold: 32.0,
        };
        let bytes_inter = encode_rgb24_inter(&rgb, &prev, w as u32, h as u32, opts).unwrap();
        // Verify it carries an inter strip (0x1100).
        let strip_off = FRAME_HEADER_SIZE;
        assert_eq!(bytes_inter[strip_off], 0x11);
        assert_eq!(bytes_inter[strip_off + 1], 0x00);

        // Decoding should still produce the same pixels.
        let mut dec2 = CinepakDecoder::new();
        // Seed dec2 with the intra frame so its prev-frame state is set.
        let _ = dec2.decode_frame(&bytes_intra, None).unwrap();
        let f2 = dec2.decode_frame(&bytes_inter, None).unwrap();
        let p = f2.pixels();
        let s = f2.stride();
        // All pixels should still be mid-gray (within tolerance).
        for r in 0..h {
            for c in 0..w {
                let off = r * s + c * 3;
                assert!(
                    (p[off] as i32 - 128).abs() <= 12,
                    "({r},{c}) R={} expected ~128",
                    p[off]
                );
            }
        }
    }

    /// Quality knob: larger `quality` should produce strictly more
    /// codebook entries (within the 8..=256 range).
    #[test]
    fn from_quality_monotonic_codebook() {
        let q0 = EncoderOptions::from_quality(0);
        let q50 = EncoderOptions::from_quality(50);
        let q100 = EncoderOptions::from_quality(100);
        assert!(q0.v4_entries < q50.v4_entries);
        assert!(q50.v4_entries < q100.v4_entries);
        assert_eq!(q0.v4_entries, q0.v1_entries);
        assert_eq!(q100.v4_entries, 256);
        assert!(q100.skip_threshold < q0.skip_threshold);
        assert!(q100.strip_count >= q0.strip_count);
    }

    /// Quality knob: encode at q=0 (smallest codebook) and confirm
    /// roundtrip still works (lossy but correct shape).
    #[test]
    fn from_quality_encodes_at_min_q() {
        let w = 16usize;
        let h = 16usize;
        let rgb = vec![100u8; w * h * 3];
        let opts = EncoderOptions::from_quality(0);
        assert_eq!(opts.v4_entries, 8);
        let bytes = encode_rgb24(&rgb, w as u32, h as u32, opts).unwrap();
        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.width, 16);
    }

    /// Validation: skip_threshold must be finite & non-negative.
    #[test]
    fn rejects_nan_skip_threshold() {
        let opts = EncoderOptions {
            v4_entries: 8,
            v1_entries: 8,
            strip_count: 1,
            skip_threshold: f32::NAN,
        };
        let rgb = vec![0u8; 8 * 8 * 3];
        assert!(encode_rgb24(&rgb, 8, 8, opts).is_err());
    }

    /// Validation: strip_count = 0 is rejected.
    #[test]
    fn rejects_zero_strip_count() {
        let opts = EncoderOptions {
            v4_entries: 8,
            v1_entries: 8,
            strip_count: 0,
            skip_threshold: 64.0,
        };
        let rgb = vec![0u8; 8 * 8 * 3];
        assert!(encode_rgb24(&rgb, 8, 8, opts).is_err());
    }

    /// Inter-encode error path: prev-frame size mismatch.
    #[test]
    fn rejects_prev_size_mismatch() {
        let rgb_a = vec![0u8; 8 * 8 * 3];
        let bytes = encode_rgb24(&rgb_a, 8, 8, EncoderOptions::default()).unwrap();
        let mut dec = CinepakDecoder::new();
        let prev = dec.decode_frame(&bytes, None).unwrap();
        let rgb_b = vec![0u8; 16 * 16 * 3];
        let r = encode_rgb24_inter(&rgb_b, &prev, 16, 16, EncoderOptions::default());
        assert!(r.is_err());
    }
}
