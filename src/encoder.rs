//! Cinepak frame-level encoder.
//!
//! Produces conformant Cinepak bitstreams (one strip per frame, intra
//! only, mixed V1+V4 codebooks) that round-trip through this crate's
//! decoder. The encoder is **reference-grade**: it does not aim to
//! match FFmpeg's per-byte output or its rate-control behaviour, only
//! to emit syntactically-valid Cinepak frames whose decoded pixels
//! recover the input within codebook quantisation error.
//!
//! ## Algorithm overview
//!
//! Encoding proceeds per macroblock (4×4 RGB pixel block):
//!
//! 1. Convert the input RGB block to the codec's `(Y0..Y3, U, V)`
//!    representation using the **forward** of the spec's inverse
//!    matrix from `04-yuv-rgb-matrix.md`.
//! 2. Build candidate **V1** and **V4** codebooks via a median-cut
//!    quantiser over the per-macroblock vectors of the strip
//!    (one V1 vector per MB, four V4 sub-block vectors per MB).
//! 3. Choose **V1** vs **V4** per macroblock by lower mean-squared
//!    error against the original block, breaking ties toward V1
//!    (smaller wire footprint).
//! 4. Emit the strip with `0x2000` (V4 full) + `0x2200` (V1 full)
//!    codebook chunks (or `0x2400`/`0x2600` for grayscale) and a
//!    `0x3000` mixed-intra vector chunk.
//!
//! The encoder's codebook size is configurable per-strip; the default
//! is `64` entries for V4 and `64` for V1, matching FFmpeg's `-q:v 10`
//! "default quality" point per `05-container-carriage.md` §4.2.
//!
//! ## Limitations
//!
//! - Single strip per frame (no automatic strip splitting).
//! - Intra only — no inter / skip-macroblock encoding.
//! - 12-bit YUV only via `encode_rgb24`; 8-bit grayscale via
//!   `encode_gray8`.
//! - No selective-update chunks (FFmpeg doesn't emit them either —
//!   spec §4.4).

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
    FrameHeader, RawStripHeader, FRAME_HEADER_SIZE, STRIP_HEADER_SIZE, STRIP_ID_INTRA,
};
use crate::vector::{encode_mixed_intra_payload, Mb, VECTOR_CHUNK_INTRA};

/// Encoder configuration.
#[derive(Clone, Copy, Debug)]
pub struct EncoderOptions {
    /// Number of V4 codebook entries (1..=256). Default `64`.
    pub v4_entries: u16,
    /// Number of V1 codebook entries (1..=256). Default `64`.
    pub v1_entries: u16,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        Self {
            v4_entries: 64,
            v1_entries: 64,
        }
    }
}

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

fn encode_intra_frame(
    pixels: &[u8],
    width: u32,
    height: u32,
    mode: PixelMode,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width % 4 != 0 || height % 4 != 0 {
        return Err(CinepakError::other(format!(
            "encoder: dims must be > 0 and multiples of 4; got {width}x{height}"
        )));
    }
    if !(1..=256).contains(&(opts.v4_entries as u32))
        || !(1..=256).contains(&(opts.v1_entries as u32))
    {
        return Err(CinepakError::other(
            "encoder: v4_entries / v1_entries must be in 1..=256",
        ));
    }
    let bpp = match mode {
        PixelMode::Yuv12 => 3,
        PixelMode::Gray8 => 1,
    };
    let expected = (width as usize) * (height as usize) * bpp;
    if pixels.len() < expected {
        return Err(CinepakError::other(format!(
            "encoder: input buffer {} bytes < expected {}",
            pixels.len(),
            expected
        )));
    }

    // 1) Convert per-MB to a (V4-quadruple list + V1-quadruple list).
    let mb_cols = (width / 4) as usize;
    let mb_rows = (height / 4) as usize;
    let mb_count = mb_cols * mb_rows;

    // For each MB: produce four V4 sub-vectors (the 2x2 quadrants
    // (TL, TR, BL, BR)) and one V1 vector for the whole MB.
    let mut v4_vectors: Vec<CodebookEntry> = Vec::with_capacity(mb_count * 4);
    let mut v1_vectors: Vec<CodebookEntry> = Vec::with_capacity(mb_count);
    let mut mb_v4: Vec<[CodebookEntry; 4]> = Vec::with_capacity(mb_count);
    let mut mb_v1: Vec<CodebookEntry> = Vec::with_capacity(mb_count);

    for mb_row in 0..mb_rows {
        for mb_col in 0..mb_cols {
            let py = mb_row * 4;
            let px = mb_col * 4;
            let v4 = sample_v4_block(pixels, width as usize, px, py, mode);
            let v1 = sample_v1_block(pixels, width as usize, px, py, mode);
            v4_vectors.extend_from_slice(&v4);
            v1_vectors.push(v1);
            mb_v4.push(v4);
            mb_v1.push(v1);
        }
    }

    // 2) Build codebooks via median-cut quantisation.
    let v4_codebook = median_cut(&v4_vectors, opts.v4_entries as usize, mode);
    let v1_codebook = median_cut(&v1_vectors, opts.v1_entries as usize, mode);

    // 3) Choose V1 vs V4 per MB by MSE.
    let mut mbs: Vec<Mb> = Vec::with_capacity(mb_count);
    for i in 0..mb_count {
        let (v4_idx, v4_err) = pick_v4(&mb_v4[i], &v4_codebook, opts.v4_entries as usize);
        let (v1_idx, v1_err) = pick_v1(&mb_v1[i], &v1_codebook, opts.v1_entries as usize, mode);
        // Tiebreak toward V1 (smaller).
        if v1_err <= v4_err {
            mbs.push(Mb::V1(v1_idx));
        } else {
            mbs.push(Mb::V4(v4_idx));
        }
    }

    // 4) Assemble bitstream.
    let mut chunks = Vec::new();

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
    encode_full_chunk(v4_kind, &v4_codebook, opts.v4_entries as usize, &mut chunks);
    encode_full_chunk(v1_kind, &v1_codebook, opts.v1_entries as usize, &mut chunks);

    // Vector chunk 0x3000.
    let mut vec_payload = Vec::new();
    encode_mixed_intra_payload(&mbs, &mut vec_payload)?;
    let vec_chunk_size = (CHUNK_HEADER_SIZE + vec_payload.len()) as u16;
    chunks.extend_from_slice(&VECTOR_CHUNK_INTRA.to_be_bytes());
    chunks.extend_from_slice(&vec_chunk_size.to_be_bytes());
    chunks.extend_from_slice(&vec_payload);

    // Strip header.
    let strip_size = (STRIP_HEADER_SIZE + chunks.len()) as u16;
    let raw = RawStripHeader {
        strip_id: STRIP_ID_INTRA,
        strip_size,
        y_top: 0,
        x_top: 0,
        y_bottom: height as u16,
        x_bottom: width as u16,
    };
    let mut strip = Vec::with_capacity(strip_size as usize);
    let mut hdr_buf = [0u8; STRIP_HEADER_SIZE];
    raw.encode(&mut hdr_buf);
    strip.extend_from_slice(&hdr_buf);
    strip.extend_from_slice(&chunks);

    // Frame header.
    let frame_length = (FRAME_HEADER_SIZE + strip.len()) as u32;
    let frame_hdr = FrameHeader {
        flags: 0,
        frame_length,
        width: width as u16,
        height: height as u16,
        strip_count: 1,
    };
    let mut out = Vec::with_capacity(frame_length as usize);
    let mut fhdr_buf = [0u8; FRAME_HEADER_SIZE];
    frame_hdr.encode(&mut fhdr_buf);
    out.extend_from_slice(&fhdr_buf);
    out.extend_from_slice(&strip);

    Ok(out)
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
}
