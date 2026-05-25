//! Cinepak frame-level decoder.
//!
//! Ties together the frame & strip header parser ([`crate::header`]),
//! codebook chunks ([`crate::codebook`]), vector chunks
//! ([`crate::vector`]), the YUV→RGB matrix ([`crate::yuv`]), and the
//! 4×4 macroblock expansion rules from
//! `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` §4 / §5
//! (V1 / V4 expansion) and §4 of `04-yuv-rgb-matrix.md` (grayscale
//! identity).

use crate::codebook::{
    apply_codebook_chunk_with, Codebook, CodebookChunkKind, PixelMode, UpdateStyle, WhichCodebook,
    CHUNK_HEADER_SIZE,
};
use crate::error::{CinepakError, Result};
use crate::header::{
    FrameHeader, RawStripHeader, StripHeader, FRAME_HEADER_SIZE, STRIP_HEADER_SIZE,
};
use crate::image::{CinepakFrame, CinepakPixelFormat, CinepakPlane};
use crate::vector::{decode_vector_chunk, Mb};
use crate::yuv::yuv_to_rgb;

/// Wire-level deviation profile for a Cinepak frame.
///
/// Standard Cinepak (FFmpeg `cvid`, QuickTime `cvid`, AVI `cvid`) has
/// a 10-byte frame header followed immediately by the first strip
/// header, and codebook chunks always declare a payload size that is a
/// clean multiple of the entry stride.
///
/// The Sega Saturn variant carried inside Sega FILM (`.cpk`) files
/// deviates in three documented ways (`Sega_FILM.wiki` lines
/// 125–143):
///
/// 1. The frame header is padded with 2 extra bytes after the standard
///    10 bytes — total prefix is **12 bytes** before the first strip.
///    The Lemmings 3DO variant pads with 6 extra bytes instead
///    (`Sega_FILM.wiki` line 189) for **16 bytes** of prefix.
/// 2. The `frame_length` field in the frame header is **8 bytes short
///    of the real frame length**. The authoritative frame size comes
///    from the FILM `STAB` sample record's `sample_length`, not from
///    the codec header.
/// 3. Codebook chunks may declare a payload size that is **not** a
///    clean multiple of the 6-byte / 4-byte entry stride; the decoder
///    is expected to truncate the entry count to
///    `floor(payload_len / entry_size)` and skip the trailing
///    remainder bytes before parsing the next chunk in the strip.
///
/// `DeviantConfig::saturn()` is the most common variant; the
/// constructor name preserves the historical association with
/// `'cvid'` data in Sega Saturn `.cpk` files even though early Sega CD
/// titles also use these deviations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviantConfig {
    /// Number of extra bytes between the standard 10-byte frame
    /// header and the first strip header (2 for Saturn / Sega CD,
    /// 6 for Lemmings 3DO).
    pub extra_header_bytes: u8,
    /// Number of bytes by which the codec-header `frame_length`
    /// field undercounts the real frame size. Saturn variant is 8.
    pub frame_length_short_by: u8,
    /// `true` if codebook chunks may carry trailing pad bytes that
    /// the decoder must skip (i.e. non-divisible payload sizes are
    /// legal). Saturn variant is `true`.
    pub tolerate_codebook_pad: bool,
}

impl DeviantConfig {
    /// Saturn / Sega CD `'cvid'` deviant variant. 12-byte total prefix
    /// (10-byte standard header + 2 extra bytes), `frame_length`
    /// short by 8, codebook chunks may have trailing pad.
    pub const fn saturn() -> Self {
        Self {
            extra_header_bytes: 2,
            frame_length_short_by: 8,
            tolerate_codebook_pad: true,
        }
    }

    /// Lemmings 3DO deviant variant. 16-byte total prefix (10-byte
    /// standard header + 6 extra bytes), `frame_length` short by 8,
    /// codebook chunks may have trailing pad.
    pub const fn lemmings_3do() -> Self {
        Self {
            extra_header_bytes: 6,
            frame_length_short_by: 8,
            tolerate_codebook_pad: true,
        }
    }
}

/// Stateful Cinepak decoder. Holds the V4 / V1 codebook pair from the
/// previous strip (for inheritance across strips and frames) and the
/// previous frame's reconstructed pixel buffer (for `0x3100` skip
/// macroblocks).
#[derive(Clone, Default)]
pub struct CinepakDecoder {
    /// V4 codebook from the previous strip (any frame).
    prev_v4: Option<Codebook>,
    /// V1 codebook from the previous strip (any frame).
    prev_v1: Option<Codebook>,
    /// Previous frame's reconstructed pixel buffer + dimensions + mode.
    /// Required for `0x3100` skip macroblocks.
    prev_frame: Option<CinepakFrame>,
}

impl CinepakDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all carry-over state. The decoder behaves as if it had
    /// never seen a frame.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Decode a single Cinepak frame from `bytes`. The frame's
    /// `frame_length` field must equal `bytes.len()` (or be smaller —
    /// the slice may carry container padding past the frame).
    pub fn decode_frame(&mut self, bytes: &[u8], pts: Option<i64>) -> Result<CinepakFrame> {
        self.decode_frame_inner(bytes, pts, None)
    }

    /// Decode a Sega Saturn / Sega CD / Lemmings 3DO **deviant**
    /// Cinepak frame from `bytes` per [`DeviantConfig`]. The caller is
    /// responsible for slicing exactly one frame's worth of bytes out
    /// of the FILM container — the codec header's `frame_length` is
    /// short by `cfg.frame_length_short_by`, so the slice's length is
    /// authoritative.
    ///
    /// Wire-format reference:
    /// `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 125–143
    /// (Saturn `'cvid'` deviation) and line 189 (Lemmings 3DO 6-byte
    /// prefix). All three deviations — extra header bytes, short
    /// `frame_length`, codebook trailing pad — are handled here.
    pub fn decode_deviant_frame(
        &mut self,
        bytes: &[u8],
        pts: Option<i64>,
        cfg: DeviantConfig,
    ) -> Result<CinepakFrame> {
        self.decode_frame_inner(bytes, pts, Some(cfg))
    }

    fn decode_frame_inner(
        &mut self,
        bytes: &[u8],
        pts: Option<i64>,
        deviant: Option<DeviantConfig>,
    ) -> Result<CinepakFrame> {
        let header = FrameHeader::parse(bytes)?;
        let strip_prefix_bytes =
            FRAME_HEADER_SIZE + deviant.map(|d| d.extra_header_bytes as usize).unwrap_or(0);
        let frame_len = match deviant {
            // Saturn deviant: header `frame_length` is short by
            // `frame_length_short_by` bytes, and the FILM container
            // sample length is authoritative — i.e. `bytes.len()`.
            // We accept the slice as-is.
            Some(_) => bytes.len(),
            None => {
                let fl = header.frame_length as usize;
                if fl > bytes.len() {
                    return Err(CinepakError::invalid(format!(
                        "frame_length {} exceeds buffer size {}",
                        fl,
                        bytes.len()
                    )));
                }
                fl
            }
        };
        let frame_bytes = &bytes[..frame_len];

        // Walk strips.
        let mut cursor = strip_prefix_bytes;
        let mut prev_y_bottom = 0u32;
        // Detected pixel mode (set by the first codebook chunk we see).
        let mut frame_mode: Option<PixelMode> = None;

        // Allocate output canvas now that we know the frame dims.
        let width = u32::from(header.width);
        let height = u32::from(header.height);

        // Mode-dependent buffer shape: until we know the pixel mode we
        // can't pick the final stride, so we allocate the RGB-shaped
        // buffer up front (the larger of the two) and lazily switch to a
        // gray-shaped buffer once the first chunk pins the mode. This
        // eliminates the post-decode byte-by-byte repack that the
        // original implementation performed on grayscale frames (the
        // round-126 baseline showed grayscale decoding at ≈ 1/3 the
        // throughput of the colour path purely because of the
        // 320×240×3 → 320×240 compaction).
        let total_px = (width as usize) * (height as usize);
        let mut out_pixels: Vec<u8> = vec![0; total_px * 3];
        let mut out_stride = (width as usize) * 3;
        let mut buffer_mode: Option<PixelMode> = None;

        // If the previous frame is the wrong size for this frame, drop
        // it — skip macroblocks must reference the *same* frame layout.
        if let Some(p) = &self.prev_frame {
            if p.width != width || p.height != height {
                self.prev_frame = None;
            }
        }

        // Take the carry-over codebooks out of `self` to avoid the
        // per-strip `Codebook::clone()` (each clone is a 256-entry × 6-byte
        // = 1.5 KiB memcpy; with 3 strips per frame that's ~9 KiB of
        // pointless copies — replaced here with a move + put-back at the
        // end of the loop body).
        let mut v4_state: Codebook = self.prev_v4.take().unwrap_or_default();
        let mut v1_state: Codebook = self.prev_v1.take().unwrap_or_default();

        for strip_index in 0..header.strip_count {
            if cursor + STRIP_HEADER_SIZE > frame_bytes.len() {
                return Err(CinepakError::invalid(format!(
                    "strip {strip_index} header truncated"
                )));
            }
            let raw = RawStripHeader::parse(&frame_bytes[cursor..cursor + STRIP_HEADER_SIZE])?;
            let strip_size = raw.strip_size as usize;
            if cursor + strip_size > frame_bytes.len() {
                return Err(CinepakError::invalid(format!(
                    "strip {strip_index} declared size {strip_size} overruns frame"
                )));
            }
            let is_first = strip_index == 0;
            let strip_hdr = StripHeader::resolve(raw, is_first, prev_y_bottom);

            // Strip bounds.
            let sx0 = u32::from(raw.x_top);
            let sx1 = u32::from(raw.x_bottom);
            if sx1 > width || strip_hdr.actual_y_bottom > height {
                return Err(CinepakError::invalid(format!(
                    "strip {strip_index} bounds ({sx0}..{sx1}, {}..{}) exceed frame ({width}x{height})",
                    strip_hdr.actual_y_top, strip_hdr.actual_y_bottom
                )));
            }
            if (sx1 - sx0) % 4 != 0 || strip_hdr.height() % 4 != 0 {
                return Err(CinepakError::invalid(format!(
                    "strip {strip_index} dims ({}x{}) not multiples of 4",
                    sx1 - sx0,
                    strip_hdr.height()
                )));
            }

            // Decode strip's chunk stream.
            let payload_start = cursor + STRIP_HEADER_SIZE;
            let payload_end = cursor + strip_size;
            let strip_payload = &frame_bytes[payload_start..payload_end];

            // Mode hint for chunk-omission strips: prefer the mode of the
            // codebook the decoder already holds (set by a prior strip of
            // this frame, falling back to the previous frame's format).
            let prev_mode_hint = match frame_mode {
                Some(m) => Some(m),
                None => self.prev_frame.as_ref().map(|p| match p.pixel_format {
                    CinepakPixelFormat::Gray8 => PixelMode::Gray8,
                    CinepakPixelFormat::Rgb24 => PixelMode::Yuv12,
                }),
            };

            let (mbs, strip_mode) = decode_strip_chunks(
                strip_payload,
                &mut v4_state,
                &mut v1_state,
                strip_hdr_mb_count(&strip_hdr, sx1 - sx0),
                strip_hdr.raw.is_intra(),
                deviant.map(|d| d.tolerate_codebook_pad).unwrap_or(false),
                prev_mode_hint,
            )?;

            // Pixel-mode unification across strips of the same frame.
            frame_mode = Some(match frame_mode {
                None => strip_mode,
                Some(m) => m.unify(strip_mode)?,
            });

            // First strip pins the buffer shape. If the strip resolves to
            // `Gray8` we shrink the output buffer to width×height and
            // adjust the stride. All subsequent strips of this frame are
            // guaranteed to be the same mode (unify_mode is enforced
            // above), so the buffer-shape decision only happens once.
            if buffer_mode.is_none() {
                buffer_mode = Some(strip_mode);
                if strip_mode == PixelMode::Gray8 {
                    out_pixels.truncate(total_px);
                    out_pixels.shrink_to_fit();
                    out_pixels.fill(0);
                    out_stride = width as usize;
                }
            }

            // Render macroblocks into the output buffer. The hot path is
            // specialised per pixel mode at the top of `render_strip` so
            // the per-pixel `match` is hoisted out of the inner loop.
            render_strip(
                &mbs,
                &v4_state,
                &v1_state,
                strip_mode,
                strip_hdr.actual_y_top,
                sx0,
                sx1 - sx0,
                strip_hdr.height(),
                width,
                height,
                self.prev_frame.as_ref(),
                &mut out_pixels,
                out_stride,
            )?;

            prev_y_bottom = strip_hdr.actual_y_bottom;
            cursor = payload_end;
        }

        // Carry codebooks forward (moved out of the per-strip loop now
        // that the strip body operates on the carry-over directly).
        self.prev_v4 = Some(v4_state);
        self.prev_v1 = Some(v1_state);

        let mode = frame_mode.unwrap_or(PixelMode::Yuv12);
        let pixel_format = match mode {
            PixelMode::Yuv12 => CinepakPixelFormat::Rgb24,
            PixelMode::Gray8 => CinepakPixelFormat::Gray8,
        };

        let frame = CinepakFrame {
            width,
            height,
            pixel_format,
            pts,
            planes: vec![CinepakPlane {
                stride: out_stride,
                data: out_pixels,
            }],
        };
        self.prev_frame = Some(frame.clone());
        Ok(frame)
    }
}

fn strip_hdr_mb_count(s: &StripHeader, width: u32) -> usize {
    ((s.height() / 4) * (width / 4)) as usize
}

/// Walk a strip's chunk stream, applying codebook chunks in the order
/// they appear and dispatching the (single) vector chunk to the
/// vector-chunk decoder. Returns the decoded macroblock list together
/// with the strip's pixel mode.
///
/// `prev_mode_hint` carries the pixel mode of the previous frame (when
/// one exists), so a strip that omits all codebook chunks — legal on an
/// inter strip that inherits the prior strip's / frame's codebook (spec
/// §3.4 of `02-codebooks.md`) — resolves to the mode the inherited
/// codebook was trained in, rather than blindly defaulting to `Yuv12`.
/// Without the hint, a fully chunk-omitted grayscale inter frame (all
/// SKIP macroblocks) would be misclassified as colour and rendered to
/// `Rgb24`. The hint is only consulted when no codebook chunk in this
/// strip pins a mode.
fn decode_strip_chunks(
    payload: &[u8],
    v4: &mut Codebook,
    v1: &mut Codebook,
    mb_count: usize,
    is_intra: bool,
    tolerate_codebook_pad: bool,
    prev_mode_hint: Option<PixelMode>,
) -> Result<(Vec<Mb>, PixelMode)> {
    let mut p = 0usize;
    let mut mode: Option<PixelMode> = None;
    let mut mbs: Option<Vec<Mb>> = None;

    while p < payload.len() {
        if payload.len() - p < CHUNK_HEADER_SIZE {
            return Err(CinepakError::invalid("strip chunk header truncated"));
        }
        let chunk_id = u16::from_be_bytes([payload[p], payload[p + 1]]);
        let chunk_size = u16::from_be_bytes([payload[p + 2], payload[p + 3]]) as usize;
        if chunk_size < CHUNK_HEADER_SIZE {
            return Err(CinepakError::invalid(format!(
                "chunk_size {chunk_size} smaller than 4-byte header"
            )));
        }
        if payload.len() - p < chunk_size {
            return Err(CinepakError::invalid(format!(
                "chunk 0x{chunk_id:04x} truncated: declared {chunk_size}, have {}",
                payload.len() - p
            )));
        }
        let chunk_payload = &payload[p + CHUNK_HEADER_SIZE..p + chunk_size];
        if let Some(kind) = CodebookChunkKind::from_id(chunk_id) {
            // Selective updates aren't legal on intra strips per spec
            // §2.3 of 02-codebooks.md.
            if is_intra && kind.style == UpdateStyle::Selective {
                return Err(CinepakError::invalid(format!(
                    "selective-update chunk 0x{chunk_id:04x} on intra strip"
                )));
            }
            // Consistency: if a previous chunk pinned a mode, keep it.
            mode = Some(match mode {
                Some(m) => m.unify(kind.mode)?,
                None => kind.mode,
            });
            let cb = match kind.which {
                WhichCodebook::V4 => &mut *v4,
                WhichCodebook::V1 => &mut *v1,
            };
            apply_codebook_chunk_with(kind, chunk_payload, cb, tolerate_codebook_pad)?;
        } else if matches!(chunk_id, 0x3000 | 0x3100 | 0x3200) {
            if mbs.is_some() {
                return Err(CinepakError::invalid(
                    "strip carries more than one vector chunk",
                ));
            }
            // Inter `0x3100` may legally only appear on inter strips
            // (spec §2 of 03-vectors-and-macroblocks.md).
            if chunk_id == 0x3100 && is_intra {
                return Err(CinepakError::invalid(
                    "inter vector chunk 0x3100 on intra strip",
                ));
            }
            mbs = Some(decode_vector_chunk(chunk_id, chunk_payload, mb_count)?);
        } else {
            return Err(CinepakError::invalid(format!(
                "unknown strip chunk id 0x{chunk_id:04x}"
            )));
        }
        p += chunk_size;
    }

    let mbs = mbs.ok_or_else(|| CinepakError::invalid("strip carries no vector chunk"))?;
    // If no codebook chunks appeared (legal when both codebooks are
    // inherited — spec §3.4 of 02: header-only / omitted codebook chunks
    // reuse the previous strip's / frame's codebook of the same flavour),
    // the strip's pixel mode isn't pinned by anything in this strip. Fall
    // back to the previous frame's mode (when known), because an inherited
    // codebook carries the mode it was trained in. This is what lets a
    // fully chunk-omitted grayscale inter frame (all SKIP macroblocks)
    // stay `Gray8` instead of being misclassified as colour. With no
    // previous-frame hint (the first frame, or a frame with no inheritable
    // codebook) we keep the historical `Yuv12` default; rendering uses the
    // zero-init / inherited codebook either way.
    let mode = mode.or(prev_mode_hint).unwrap_or(PixelMode::Yuv12);
    Ok((mbs, mode))
}

/// Render every macroblock in `mbs` into `out`. Specialised per pixel
/// mode at the top of the function so the inner loop never re-tests the
/// mode — measurably (round-129 decoder benches): hoisting the
/// `PixelMode` match out of `write_pixel` (which used to run 16 times
/// per macroblock, ≥ 1 200 times per 64×64 frame) is most of the
/// per-frame speed-up. The two paths are otherwise structurally
/// identical; the body just differs in how it writes a `(y, u, v)`
/// triple into the destination.
#[allow(clippy::too_many_arguments)]
fn render_strip(
    mbs: &[Mb],
    v4: &Codebook,
    v1: &Codebook,
    mode: PixelMode,
    y_top: u32,
    x_top: u32,
    width: u32,
    _height: u32,
    frame_width: u32,
    frame_height: u32,
    prev_frame: Option<&CinepakFrame>,
    out: &mut [u8],
    out_stride: usize,
) -> Result<()> {
    let mb_cols = (width / 4) as usize;
    match mode {
        PixelMode::Yuv12 => render_strip_rgb(
            mbs,
            v4,
            v1,
            mb_cols,
            y_top,
            x_top,
            frame_width,
            frame_height,
            prev_frame,
            out,
            out_stride,
        ),
        PixelMode::Gray8 => render_strip_gray(
            mbs,
            v4,
            v1,
            mb_cols,
            y_top,
            x_top,
            frame_width,
            frame_height,
            prev_frame,
            out,
            out_stride,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_strip_rgb(
    mbs: &[Mb],
    v4: &Codebook,
    v1: &Codebook,
    mb_cols: usize,
    y_top: u32,
    x_top: u32,
    _frame_width: u32,
    _frame_height: u32,
    prev_frame: Option<&CinepakFrame>,
    out: &mut [u8],
    out_stride: usize,
) -> Result<()> {
    for (i, mb) in mbs.iter().enumerate() {
        let mb_row = i / mb_cols;
        let mb_col = i % mb_cols;
        let py = y_top as usize + mb_row * 4;
        let px = x_top as usize + mb_col * 4;
        match mb {
            Mb::V1(idx) => {
                let e = &v1.entries[*idx as usize];
                draw_v1_mb_rgb(e, px, py, out, out_stride);
            }
            Mb::V4(idx) => {
                // SAFETY-style note: indexing with `u8` is bounds-checked
                // against `[CodebookEntry; 256]` — the array length is
                // exactly the index range so the compiler elides the
                // panic edges.
                let r0 = &v4.entries[idx[0] as usize];
                let r1 = &v4.entries[idx[1] as usize];
                let r2 = &v4.entries[idx[2] as usize];
                let r3 = &v4.entries[idx[3] as usize];
                draw_v4_mb_rgb([r0, r1, r2, r3], px, py, out, out_stride);
            }
            Mb::Skip => {
                let prev = prev_frame.ok_or_else(|| {
                    CinepakError::invalid(
                        "0x3100 skip macroblock on first frame (no previous reconstruction)",
                    )
                })?;
                copy_mb_from_prev_rgb(prev, px, py, out, out_stride);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_strip_gray(
    mbs: &[Mb],
    v4: &Codebook,
    v1: &Codebook,
    mb_cols: usize,
    y_top: u32,
    x_top: u32,
    _frame_width: u32,
    _frame_height: u32,
    prev_frame: Option<&CinepakFrame>,
    out: &mut [u8],
    out_stride: usize,
) -> Result<()> {
    for (i, mb) in mbs.iter().enumerate() {
        let mb_row = i / mb_cols;
        let mb_col = i % mb_cols;
        let py = y_top as usize + mb_row * 4;
        let px = x_top as usize + mb_col * 4;
        match mb {
            Mb::V1(idx) => {
                let e = &v1.entries[*idx as usize];
                draw_v1_mb_gray(e, px, py, out, out_stride);
            }
            Mb::V4(idx) => {
                let r0 = &v4.entries[idx[0] as usize];
                let r1 = &v4.entries[idx[1] as usize];
                let r2 = &v4.entries[idx[2] as usize];
                let r3 = &v4.entries[idx[3] as usize];
                draw_v4_mb_gray([r0, r1, r2, r3], px, py, out, out_stride);
            }
            Mb::Skip => {
                let prev = prev_frame.ok_or_else(|| {
                    CinepakError::invalid(
                        "0x3100 skip macroblock on first frame (no previous reconstruction)",
                    )
                })?;
                copy_mb_from_prev_gray(prev, px, py, out, out_stride);
            }
        }
    }
    Ok(())
}

/// V1 macroblock — RGB path. Each Yi covers a 2×2 quadrant, with the
/// same (U, V) across the whole 4×4. We precompute the four `(R,G,B)`
/// triples once (4 calls to `yuv_to_rgb` instead of the original 16),
/// then write each quadrant as a pair of identical row halves.
#[inline]
fn draw_v1_mb_rgb(
    e: &crate::codebook::CodebookEntry,
    px: usize,
    py: usize,
    out: &mut [u8],
    stride: usize,
) {
    // (U, V) are constant across the whole 4×4.
    let p_tl = yuv_to_rgb(e.y0, e.u, e.v);
    let p_tr = yuv_to_rgb(e.y1, e.u, e.v);
    let p_bl = yuv_to_rgb(e.y2, e.u, e.v);
    let p_br = yuv_to_rgb(e.y3, e.u, e.v);
    // Build a 12-byte template for the upper row pair (Y0 Y0 Y1 Y1) and
    // the lower row pair (Y2 Y2 Y3 Y3) once, then `copy_from_slice` it
    // into each of the two duplicated rows of each pair.
    let top: [u8; 12] = [
        p_tl.0, p_tl.1, p_tl.2, p_tl.0, p_tl.1, p_tl.2, p_tr.0, p_tr.1, p_tr.2, p_tr.0, p_tr.1,
        p_tr.2,
    ];
    let bot: [u8; 12] = [
        p_bl.0, p_bl.1, p_bl.2, p_bl.0, p_bl.1, p_bl.2, p_br.0, p_br.1, p_br.2, p_br.0, p_br.1,
        p_br.2,
    ];
    let row0 = py * stride + px * 3;
    let row1 = row0 + stride;
    let row2 = row1 + stride;
    let row3 = row2 + stride;
    out[row0..row0 + 12].copy_from_slice(&top);
    out[row1..row1 + 12].copy_from_slice(&top);
    out[row2..row2 + 12].copy_from_slice(&bot);
    out[row3..row3 + 12].copy_from_slice(&bot);
}

/// V4 macroblock — RGB path. Each codebook entry covers a 2×2
/// sub-block of the 4×4 (TL, TR, BL, BR). Within each 2×2 the 4
/// Y samples (Y0..Y3) cover (row=0,col=0), (0,1), (1,0), (1,1) — i.e.
/// row-major scan — with one shared (U, V) per entry.
///
/// We precompute the 4 `(R,G,B)` triples per sub-block (4 entries × 4
/// triples = 16 conversions per MB, same as the V1 path); then write
/// the two 6-byte row halves of each sub-block directly into the
/// output buffer.
#[inline]
fn draw_v4_mb_rgb(
    rs: [&crate::codebook::CodebookEntry; 4],
    px: usize,
    py: usize,
    out: &mut [u8],
    stride: usize,
) {
    // For each sub-block, build the 6-byte top row (Y0 Y1) and the
    // 6-byte bottom row (Y2 Y3). One conversion per sample.
    let row0 = py * stride + px * 3;
    let row1 = row0 + stride;
    let row2 = row1 + stride;
    let row3 = row2 + stride;
    let mut emit_subblock =
        |sub: &crate::codebook::CodebookEntry, top_row_off: usize, bot_row_off: usize| {
            let (r0, g0, b0) = yuv_to_rgb(sub.y0, sub.u, sub.v);
            let (r1, g1, b1) = yuv_to_rgb(sub.y1, sub.u, sub.v);
            let (r2, g2, b2) = yuv_to_rgb(sub.y2, sub.u, sub.v);
            let (r3, g3, b3) = yuv_to_rgb(sub.y3, sub.u, sub.v);
            let top: [u8; 6] = [r0, g0, b0, r1, g1, b1];
            let bot: [u8; 6] = [r2, g2, b2, r3, g3, b3];
            out[top_row_off..top_row_off + 6].copy_from_slice(&top);
            out[bot_row_off..bot_row_off + 6].copy_from_slice(&bot);
        };
    // r0 = TL (rows 0..1, cols 0..1)
    emit_subblock(rs[0], row0, row1);
    // r1 = TR (rows 0..1, cols 2..3)
    emit_subblock(rs[1], row0 + 6, row1 + 6);
    // r2 = BL (rows 2..3, cols 0..1)
    emit_subblock(rs[2], row2, row3);
    // r3 = BR (rows 2..3, cols 2..3)
    emit_subblock(rs[3], row2 + 6, row3 + 6);
}

/// V1 macroblock — grayscale path. (U, V) are ignored; we expand the
/// 4 Y samples into a 4×4 luminance plane with row-strided byte writes.
#[inline]
fn draw_v1_mb_gray(
    e: &crate::codebook::CodebookEntry,
    px: usize,
    py: usize,
    out: &mut [u8],
    stride: usize,
) {
    let top: [u8; 4] = [e.y0, e.y0, e.y1, e.y1];
    let bot: [u8; 4] = [e.y2, e.y2, e.y3, e.y3];
    let row0 = py * stride + px;
    let row1 = row0 + stride;
    let row2 = row1 + stride;
    let row3 = row2 + stride;
    out[row0..row0 + 4].copy_from_slice(&top);
    out[row1..row1 + 4].copy_from_slice(&top);
    out[row2..row2 + 4].copy_from_slice(&bot);
    out[row3..row3 + 4].copy_from_slice(&bot);
}

/// V4 macroblock — grayscale path. Four sub-block × four-Y writes into
/// a single-byte-per-pixel buffer.
#[inline]
fn draw_v4_mb_gray(
    rs: [&crate::codebook::CodebookEntry; 4],
    px: usize,
    py: usize,
    out: &mut [u8],
    stride: usize,
) {
    let row0 = py * stride + px;
    let row1 = row0 + stride;
    let row2 = row1 + stride;
    let row3 = row2 + stride;
    let mut emit_subblock =
        |sub: &crate::codebook::CodebookEntry, top_row_off: usize, bot_row_off: usize| {
            out[top_row_off] = sub.y0;
            out[top_row_off + 1] = sub.y1;
            out[bot_row_off] = sub.y2;
            out[bot_row_off + 1] = sub.y3;
        };
    emit_subblock(rs[0], row0, row1);
    emit_subblock(rs[1], row0 + 2, row1 + 2);
    emit_subblock(rs[2], row2, row3);
    emit_subblock(rs[3], row2 + 2, row3 + 2);
}

/// Skip macroblock — RGB path. Previous frame must also be RGB-shaped
/// (mode unification guarantees this).
#[inline]
fn copy_mb_from_prev_rgb(prev: &CinepakFrame, px: usize, py: usize, out: &mut [u8], stride: usize) {
    let prev_stride = prev.planes[0].stride;
    let prev_data = &prev.planes[0].data;
    if prev.pixel_format != CinepakPixelFormat::Rgb24 {
        // Mode-unification mismatch — should never happen. Fill with
        // zeros defensively.
        for dy in 0..4 {
            let off = (py + dy) * stride + px * 3;
            for byte in &mut out[off..off + 12] {
                *byte = 0;
            }
        }
        return;
    }
    for dy in 0..4 {
        let prev_off = (py + dy) * prev_stride + px * 3;
        let out_off = (py + dy) * stride + px * 3;
        out[out_off..out_off + 12].copy_from_slice(&prev_data[prev_off..prev_off + 12]);
    }
}

/// Skip macroblock — grayscale path. Previous frame must also be
/// `Gray8`-shaped.
#[inline]
fn copy_mb_from_prev_gray(
    prev: &CinepakFrame,
    px: usize,
    py: usize,
    out: &mut [u8],
    stride: usize,
) {
    let prev_stride = prev.planes[0].stride;
    let prev_data = &prev.planes[0].data;
    if prev.pixel_format != CinepakPixelFormat::Gray8 {
        for dy in 0..4 {
            let off = (py + dy) * stride + px;
            for byte in &mut out[off..off + 4] {
                *byte = 0;
            }
        }
        return;
    }
    for dy in 0..4 {
        let prev_off = (py + dy) * prev_stride + px;
        let out_off = (py + dy) * stride + px;
        out[out_off..out_off + 4].copy_from_slice(&prev_data[prev_off..prev_off + 4]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a synthesised V1-only intra frame: 4×4 single MB with
    /// known luminance values. Verifies the V1 quadrant expansion.
    #[test]
    fn decodes_minimal_v1_only_intra_frame() {
        // 4×4 frame, 1 strip, 1 MB.
        // Codebook chunks: empty V4 (header-only) + V1 with one entry
        // (Y0=50, Y1=100, Y2=150, Y3=200, U=0, V=0).
        // Vector chunk: 0x3200 with one byte (V1 index 0).
        let mut bytes = Vec::new();
        // Frame header (10 bytes): flags=0, frame_length=?, w=4, h=4, sc=1
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 4, 0, 4, 0, 1]);
        // Strip header (12 bytes): strip_id=0x1000, strip_size=?, y_top=0, x_top=0, y_bottom=4, x_bottom=4
        let strip_hdr_pos = bytes.len();
        bytes.extend_from_slice(&[0x10, 0x00, 0, 0, 0, 0, 0, 0, 0, 4, 0, 4]);
        // Codebook chunks: header-only V4 full + V1 full with 1 entry.
        bytes.extend_from_slice(&[0x20, 0x00, 0x00, 0x04]); // 0x2000, size=4
        bytes.extend_from_slice(&[0x22, 0x00, 0x00, 0x0a]); // 0x2200, size=10
        bytes.extend_from_slice(&[50, 100, 150, 200, 0, 0]);
        // Vector chunk 0x3200 with 1 MB.
        bytes.extend_from_slice(&[0x32, 0x00, 0x00, 0x05]); // 0x3200, size=5
        bytes.push(0); // V1 index 0

        // Patch strip_size and frame_length.
        let strip_size = (bytes.len() - strip_hdr_pos) as u16;
        bytes[strip_hdr_pos + 2..strip_hdr_pos + 4].copy_from_slice(&strip_size.to_be_bytes());
        let frame_len = bytes.len() as u32;
        bytes[1] = ((frame_len >> 16) & 0xff) as u8;
        bytes[2] = ((frame_len >> 8) & 0xff) as u8;
        bytes[3] = (frame_len & 0xff) as u8;

        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.width, 4);
        assert_eq!(f.height, 4);
        assert_eq!(f.pixel_format, CinepakPixelFormat::Rgb24);
        // Y0=50 covers TL 2×2 → rows 0..1, cols 0..1.
        // RGB for U=V=0: R = Y, G = Y, B = Y.
        let p = f.pixels();
        // Row 0, col 0: (50, 50, 50)
        assert_eq!(p[0], 50);
        assert_eq!(p[1], 50);
        assert_eq!(p[2], 50);
        // Row 0, col 2: Y1=100
        assert_eq!(p[6], 100);
        // Row 2, col 0: Y2=150
        let row2_off = 2 * f.stride();
        assert_eq!(p[row2_off], 150);
        // Row 2, col 2: Y3=200
        assert_eq!(p[row2_off + 6], 200);
    }
}
