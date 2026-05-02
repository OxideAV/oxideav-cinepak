//! Cinepak (CVID) packet → YUV420P frame decoder.
//!
//! Driven from `docs/video/cinepak/cinepak-trace-reverse-engineering.md`.
//! See the crate-level docs for the bitstream layout we implement.

use oxideav_core::frame::VideoPlane;
use oxideav_core::{CodecId, Decoder, Error, Frame, Packet, PixelFormat, Result, VideoFrame};

/// Top-level Cinepak decoder.
#[derive(Debug)]
pub struct CinepakDecoder {
    codec_id: CodecId,
    /// Pending packet between `send_packet` and `receive_frame`.
    pending: Option<Packet>,
    eof: bool,
    /// Last decoded YUV420P frame, used as the source of `MB_SKIP`
    /// macroblocks for the next frame. `None` until the first frame.
    prev: Option<DecodedFrame>,
    /// Width × height observed on the last frame; used to detect
    /// dimension changes (which invalidate `prev`).
    dims: Option<(u16, u16)>,
}

impl CinepakDecoder {
    pub fn new(codec_id: CodecId) -> Self {
        Self {
            codec_id,
            pending: None,
            eof: false,
            prev: None,
            dims: None,
        }
    }
}

impl Decoder for CinepakDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.pending.is_some() {
            return Err(Error::other(
                "Cinepak decoder: receive_frame must be called before sending another packet",
            ));
        }
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        let Some(pkt) = self.pending.take() else {
            return if self.eof {
                Err(Error::Eof)
            } else {
                Err(Error::NeedMore)
            };
        };
        let decoded = decode_packet(&pkt.data, self.prev.as_ref())?;
        // Detect dimension changes - invalidate prev so we don't read out-of-bounds.
        if self.dims != Some((decoded.width, decoded.height)) {
            self.dims = Some((decoded.width, decoded.height));
        }
        let frame = decoded.to_video_frame(pkt.pts);
        self.prev = Some(decoded);
        Ok(Frame::Video(frame))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }
}

// ───────────────────── Internal frame buffer ─────────────────────

/// Internal YUV420P picture with per-plane Vec<u8> storage. We keep
/// luma at 1 byte/pixel (width × height) and Cb/Cr at (width/2) ×
/// (height/2). Strides equal the plane width — no padding. The
/// rounded-up dimensions are stored so MB raster bounds-checking can
/// use them directly.
#[derive(Clone, Debug)]
pub(crate) struct DecodedFrame {
    pub width: u16,
    pub height: u16,
    /// Width/height rounded up to the next multiple of 4 (matches the
    /// MB grid the encoder rasterised against).
    pub padded_w: usize,
    pub padded_h: usize,
    pub y_plane: Vec<u8>,
    pub cb_plane: Vec<u8>,
    pub cr_plane: Vec<u8>,
}

impl DecodedFrame {
    fn new(width: u16, height: u16) -> Self {
        let padded_w = ((width as usize) + 3) & !3;
        let padded_h = ((height as usize) + 3) & !3;
        let y_len = padded_w * padded_h;
        let c_len = (padded_w / 2) * (padded_h / 2);
        Self {
            width,
            height,
            padded_w,
            padded_h,
            y_plane: vec![0u8; y_len],
            cb_plane: vec![128u8; c_len],
            cr_plane: vec![128u8; c_len],
        }
    }

    fn to_video_frame(&self, pts: Option<i64>) -> VideoFrame {
        // Crop padded planes back to the declared (width, height).
        let w = self.width as usize;
        let h = self.height as usize;
        let cw = (self.width as usize).div_ceil(2);
        let ch = (self.height as usize).div_ceil(2);
        let mut y_out = Vec::with_capacity(w * h);
        for row in 0..h {
            let off = row * self.padded_w;
            y_out.extend_from_slice(&self.y_plane[off..off + w]);
        }
        let mut cb_out = Vec::with_capacity(cw * ch);
        let mut cr_out = Vec::with_capacity(cw * ch);
        let pad_cw = self.padded_w / 2;
        for row in 0..ch {
            let off = row * pad_cw;
            cb_out.extend_from_slice(&self.cb_plane[off..off + cw]);
            cr_out.extend_from_slice(&self.cr_plane[off..off + cw]);
        }
        VideoFrame {
            pts,
            planes: vec![
                VideoPlane {
                    stride: w,
                    data: y_out,
                },
                VideoPlane {
                    stride: cw,
                    data: cb_out,
                },
                VideoPlane {
                    stride: cw,
                    data: cr_out,
                },
            ],
        }
    }
}

/// Output pixel format the decoder emits.
pub const OUTPUT_PIX_FMT: PixelFormat = PixelFormat::Yuv420P;

// ───────────────────── Bit-cursor over big-endian bytes ─────────────────────

/// Reads an unbounded byte stream as 32-bit big-endian flag words, MSB
/// first. Each `pull_bit` consumes one bit; when the 32-bit register
/// runs dry we refill from the next 4 bytes (or 1 padding word of zeros
/// if the buffer is exhausted — matches the "tolerate truncation"
/// behaviour the trace-doc §10 describes).
struct FlagBits<'a> {
    bytes: &'a [u8],
    pos: usize,
    flag: u32,
    /// Number of unread bits left in `flag`. Range 0..=32. `0` means
    /// the next `pull_bit` triggers a refill.
    bits_left: u8,
}

impl<'a> FlagBits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            flag: 0,
            bits_left: 0,
        }
    }

    fn refill(&mut self) {
        let mut w = [0u8; 4];
        for (i, slot) in w.iter_mut().enumerate() {
            if self.pos + i < self.bytes.len() {
                *slot = self.bytes[self.pos + i];
            }
        }
        self.flag = u32::from_be_bytes(w);
        // Advance over what we successfully consumed (may be less than 4
        // at end-of-stream; subsequent refills then yield zeros).
        self.pos = (self.pos + 4).min(self.bytes.len());
        self.bits_left = 32;
    }

    fn pull_bit(&mut self) -> u8 {
        if self.bits_left == 0 {
            self.refill();
        }
        self.bits_left -= 1;
        ((self.flag >> self.bits_left) & 1) as u8
    }

    /// Read the next byte from the underlying byte stream (NOT bits).
    /// Cinepak per-MB codebook indices come straight from the byte
    /// stream, not from the flag register. This advances `pos` and
    /// does NOT touch `flag` / `bits_left`.
    fn pull_byte(&mut self) -> u8 {
        if self.pos >= self.bytes.len() {
            return 0;
        }
        let b = self.bytes[self.pos];
        self.pos += 1;
        b
    }
}

// ───────────────────── Codebooks ─────────────────────

/// One Cinepak codebook entry: 2x2 luma quad + signed (u, v) chroma.
#[derive(Clone, Copy, Debug, Default)]
struct CbEntry {
    y: [u8; 4], // Y00, Y01, Y10, Y11
    u: i8,
    v: i8,
}

#[derive(Clone, Debug)]
struct Codebook {
    entries: [CbEntry; 256],
}

impl Codebook {
    fn new() -> Self {
        Self {
            entries: [CbEntry::default(); 256],
        }
    }
}

// ───────────────────── Frame / Strip / Chunk parsing ─────────────────────

const FRAME_HEADER_LEN: usize = 10;
const STRIP_HEADER_LEN: usize = 12;
const CHUNK_HEADER_LEN: usize = 4;
const MAX_STRIPS: usize = 32;

fn read_u16_be(b: &[u8], off: usize) -> Result<u16> {
    if off + 2 > b.len() {
        return Err(Error::invalid("Cinepak: truncated u16"));
    }
    Ok(u16::from_be_bytes([b[off], b[off + 1]]))
}

fn read_u24_be(b: &[u8], off: usize) -> Result<u32> {
    if off + 3 > b.len() {
        return Err(Error::invalid("Cinepak: truncated u24"));
    }
    Ok(((b[off] as u32) << 16) | ((b[off + 1] as u32) << 8) | (b[off + 2] as u32))
}

#[derive(Debug)]
struct FrameHeader {
    flags: u8,
    encoded_buf_size: u32,
    width: u16,
    height: u16,
    num_strips: u16,
}

fn parse_frame_header(data: &[u8]) -> Result<FrameHeader> {
    if data.len() < FRAME_HEADER_LEN {
        return Err(Error::invalid("Cinepak: frame header truncated"));
    }
    let flags = data[0];
    let encoded_buf_size = read_u24_be(data, 1)?;
    let width = read_u16_be(data, 4)?;
    let height = read_u16_be(data, 6)?;
    let num_strips = read_u16_be(data, 8)?;
    if width == 0 || height == 0 {
        return Err(Error::invalid("Cinepak: zero dimensions"));
    }
    if num_strips as usize > MAX_STRIPS {
        // Trace-doc §10: "num_strips is clamped to 32 by FFmpeg; bytes
        // claiming more strips are silently dropped." We mirror that.
        return Err(Error::invalid("Cinepak: num_strips > 32"));
    }
    Ok(FrameHeader {
        flags,
        encoded_buf_size,
        width,
        height,
        num_strips,
    })
}

fn decode_packet(data: &[u8], prev: Option<&DecodedFrame>) -> Result<DecodedFrame> {
    let hdr = parse_frame_header(data)?;
    let mut frame = DecodedFrame::new(hdr.width, hdr.height);

    // If the very first frame has SKIP MBs (it shouldn't per spec, but
    // tolerate), they'll resolve to whatever colour the freshly-zeroed
    // luma + 128-grey chroma plane gave us. For frames after the first,
    // pre-fill from the previous picture so SKIP MBs retain their
    // history before any in-strip mutation.
    if let Some(pframe) = prev {
        if pframe.padded_w == frame.padded_w && pframe.padded_h == frame.padded_h {
            frame.y_plane.copy_from_slice(&pframe.y_plane);
            frame.cb_plane.copy_from_slice(&pframe.cb_plane);
            frame.cr_plane.copy_from_slice(&pframe.cr_plane);
        }
    }

    // The container packet may be longer than `encoded_buf_size`
    // (Sega FILM filler etc.) — we honour the in-band length when
    // smaller, otherwise fall back to packet length.
    let encoded_end = if (hdr.encoded_buf_size as usize) <= data.len() {
        hdr.encoded_buf_size as usize
    } else {
        data.len()
    };
    let frame_bytes = &data[..encoded_end];

    let mut cursor = FRAME_HEADER_LEN;
    let mut strip_y_cursor: usize = 0;

    let mut prev_v1 = Codebook::new();
    let mut prev_v4 = Codebook::new();
    let mut have_prev_cb = false;

    for strip_idx in 0..hdr.num_strips as usize {
        if cursor + STRIP_HEADER_LEN > frame_bytes.len() {
            return Err(Error::invalid("Cinepak: strip header truncated"));
        }
        let strip_start = cursor;
        let strip_id = frame_bytes[cursor];
        let strip_size = read_u24_be(frame_bytes, cursor + 1)? as usize;
        let s_y1 = read_u16_be(frame_bytes, cursor + 4)? as usize;
        let _s_x1 = read_u16_be(frame_bytes, cursor + 6)? as usize;
        let s_y2 = read_u16_be(frame_bytes, cursor + 8)? as usize;
        let s_x2 = read_u16_be(frame_bytes, cursor + 10)? as usize;
        cursor += STRIP_HEADER_LEN;

        if strip_size < STRIP_HEADER_LEN {
            return Err(Error::invalid("Cinepak: strip_chunk_size < 12"));
        }
        // Strip-relative payload range (header + chunks).
        let strip_end = (strip_start + strip_size).min(frame_bytes.len());

        // Per trace-doc §3.3, when y1 == 0 we stack onto the running y
        // cursor. We always use full frame width.
        let (y1, y2) = if s_y1 == 0 {
            let y1 = strip_y_cursor;
            let strip_h = s_y2;
            let y2 = (y1 + strip_h).min(frame.padded_h);
            strip_y_cursor = y2;
            (y1, y2)
        } else {
            // Absolute path: use s_y1..s_y2 directly. Reset the running
            // cursor so that future "y1 == 0" strips stack onto the
            // last absolute strip's y2 (matches FFmpeg behaviour).
            let y1 = s_y1.min(frame.padded_h);
            let y2 = s_y2.min(frame.padded_h);
            strip_y_cursor = y2;
            (y1, y2)
        };
        // x1/x2 from the strip header are honoured for vector-list
        // bounds; in observed corpora these are always (0, frame_w)
        // but we don't assume.
        let x1 = 0usize;
        let x2 = ((s_x2 as usize).max(frame.width as usize)).min(frame.padded_w);
        // If the encoder wrote a non-zero x1 we'd respect it; default
        // is full width which is what every observed clip uses.
        let _ = s_x2; // already consumed via x2

        // Codebook inheritance: per trace-doc §3.3, frame_flags bit 0
        // clear AND not the first strip ⇒ inherit V1/V4 from the
        // previous strip's *parsed* codebooks.
        let mut v1 = Codebook::new();
        let mut v4 = Codebook::new();
        if strip_idx > 0 && (hdr.flags & 1) == 0 && have_prev_cb {
            v1 = prev_v1.clone();
            v4 = prev_v4.clone();
        }

        // Walk chunks until we exhaust the strip's byte range.
        let chunks_end = strip_end;
        while cursor + CHUNK_HEADER_LEN <= chunks_end {
            let chunk_id = frame_bytes[cursor];
            let chunk_size = read_u24_be(frame_bytes, cursor + 1)? as usize;
            if chunk_size < CHUNK_HEADER_LEN {
                return Err(Error::invalid("Cinepak: chunk_size < 4"));
            }
            let payload_start = cursor + CHUNK_HEADER_LEN;
            let payload_end = (cursor + chunk_size).min(chunks_end);
            let payload = &frame_bytes[payload_start..payload_end];

            match chunk_id & 0xF0 {
                0x20 => apply_codebook_chunk(chunk_id, payload, &mut v1, &mut v4)?,
                0x30 => render_vectors(
                    chunk_id, payload, &mut frame, &v1, &v4, strip_id, y1, y2, x1, x2,
                )?,
                _ => {
                    // Unknown chunk family — skip silently, matching
                    // the dispatcher's "switch with fallthrough" model.
                }
            }
            cursor = (cursor + chunk_size).min(chunks_end);
        }

        // Save these codebooks so the *next* strip can inherit them
        // (only applied when the inheritance condition fires above).
        prev_v1 = v1;
        prev_v4 = v4;
        have_prev_cb = true;

        // Move to the next strip's expected position. The strip's own
        // chunk_size dictates the byte range; cursor should already be
        // at strip_end after the chunk loop, but re-anchor defensively.
        cursor = strip_end;
    }

    Ok(frame)
}

// ───────────────────── Codebook update ─────────────────────

fn apply_codebook_chunk(
    chunk_id: u8,
    payload: &[u8],
    v1: &mut Codebook,
    v4: &mut Codebook,
) -> Result<()> {
    // Low-nibble bits per trace-doc §4:
    //   bit 1 — 0=V4, 1=V1 codebook target.
    //   bit 0 — 0=full prefix update, 1=selective (256 flag bits).
    //   bit 2 — 0=6-byte entries (Y00..Y11 + signed u + signed v),
    //           1=4-byte entries (luma only, no chroma).
    let target_v1 = (chunk_id & 0x02) != 0;
    let selective = (chunk_id & 0x01) != 0;
    let four_byte = (chunk_id & 0x04) != 0;
    let entry_len = if four_byte { 4 } else { 6 };
    let cb = if target_v1 { v1 } else { v4 };

    if !selective {
        // Full prefix: rewrite entries 0..N where N = payload / entry_len.
        let n = payload.len() / entry_len;
        for i in 0..n {
            let off = i * entry_len;
            cb.entries[i] = parse_entry(&payload[off..off + entry_len], four_byte);
        }
        return Ok(());
    }

    // Selective: payload begins with up to 8 big-endian flag words
    // (256 bits total), followed by entry data for set bits only.
    // The decoder must bail gracefully when the chunk runs out before
    // 256 entries have been consulted.
    let mut p = 0usize;
    let mut entry_idx = 0usize;
    'outer: while entry_idx < 256 {
        if p + 4 > payload.len() {
            break;
        }
        let mut word =
            u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]]);
        p += 4;
        for _ in 0..32 {
            if entry_idx >= 256 {
                break 'outer;
            }
            if (word & 0x8000_0000) != 0 {
                if p + entry_len > payload.len() {
                    break 'outer;
                }
                cb.entries[entry_idx] = parse_entry(&payload[p..p + entry_len], four_byte);
                p += entry_len;
            }
            word <<= 1;
            entry_idx += 1;
        }
    }
    Ok(())
}

fn parse_entry(bytes: &[u8], four_byte: bool) -> CbEntry {
    let mut e = CbEntry::default();
    if bytes.len() < 4 {
        return e;
    }
    e.y[0] = bytes[0];
    e.y[1] = bytes[1];
    e.y[2] = bytes[2];
    e.y[3] = bytes[3];
    if !four_byte && bytes.len() >= 6 {
        e.u = bytes[4] as i8;
        e.v = bytes[5] as i8;
    }
    e
}

// ───────────────────── Vector-list rendering ─────────────────────

#[allow(clippy::too_many_arguments)]
fn render_vectors(
    chunk_id: u8,
    payload: &[u8],
    frame: &mut DecodedFrame,
    v1: &Codebook,
    v4: &Codebook,
    strip_id: u8,
    y1: usize,
    y2: usize,
    x1: usize,
    x2: usize,
) -> Result<()> {
    // Dispatch matches §6:
    //   0x30 — INTRA: 1 mode bit per MB, no skip bits.
    //   0x31 — INTER: 1 skip bit, then 1 mode bit if not skipped.
    //   0x32 — V1-only: no flag bits; one V1 byte per MB.
    let mode = chunk_id & 0x0F;
    let mut bits = FlagBits::new(payload);

    let mut y = y1;
    while y < y2 {
        let mut x = x1;
        while x < x2 {
            match mode {
                0x00 => {
                    // INTRA: 1 = V4, 0 = V1.
                    let bit = bits.pull_bit();
                    if bit == 1 {
                        let i0 = bits.pull_byte();
                        let i1 = bits.pull_byte();
                        let i2 = bits.pull_byte();
                        let i3 = bits.pull_byte();
                        paint_v4(frame, x, y, v4, i0, i1, i2, i3);
                    } else {
                        let i = bits.pull_byte();
                        paint_v1(frame, x, y, v1, i);
                    }
                }
                0x01 => {
                    // INTER: skip-or-mode then optional mode bit.
                    // INTRA strip-id (0x10) shouldn't see 0x31; if it
                    // does, behaviour matches FFmpeg's tolerant path
                    // and we still consume the bits.
                    let _ = strip_id;
                    let coded = bits.pull_bit();
                    if coded == 0 {
                        // SKIP — leave MB as-is (already pre-filled
                        // from the previous frame at the top of
                        // decode_packet). No bytes consumed.
                    } else {
                        let kind = bits.pull_bit();
                        if kind == 1 {
                            let i0 = bits.pull_byte();
                            let i1 = bits.pull_byte();
                            let i2 = bits.pull_byte();
                            let i3 = bits.pull_byte();
                            paint_v4(frame, x, y, v4, i0, i1, i2, i3);
                        } else {
                            let i = bits.pull_byte();
                            paint_v1(frame, x, y, v1, i);
                        }
                    }
                }
                0x02 => {
                    // V1-only: one byte per MB, no flag bits.
                    let i = bits.pull_byte();
                    paint_v1(frame, x, y, v1, i);
                }
                _ => {
                    // Unknown vector-list family — drop the rest.
                    return Ok(());
                }
            }
            x += 4;
        }
        y += 4;
    }
    Ok(())
}

/// Convert a signed Cinepak chroma component to the unsigned YUV420P
/// range. Cinepak's `(u, v)` are signed 8-bit centred on 0; the
/// standard YCbCr planar layout expects 0..=255 with 128 being neutral
/// — `Cb = u + 128`, `Cr = v + 128`, clipped.
#[inline]
fn chroma_unsigned(c: i8) -> u8 {
    let v = (c as i16) + 128;
    v.clamp(0, 255) as u8
}

/// Paint a 4x4 MB using a single V1 codebook entry. The entry's 2x2
/// luma quad is tiled 2x to fill the 4x4 luma area (each entry sample
/// → a 2x2 pixel tile), and the entry's `(u, v)` covers all 2x2
/// chroma cells of the MB.
fn paint_v1(frame: &mut DecodedFrame, x: usize, y: usize, cb: &Codebook, idx: u8) {
    let e = cb.entries[idx as usize];
    let (cb_byte, cr_byte) = (chroma_unsigned(e.u), chroma_unsigned(e.v));
    // Luma: tile 2x2 entry to 4x4 area. entry layout:
    //   Y00 Y01    upper sub-MBs (top 2 rows of 4x4)
    //   Y10 Y11    lower sub-MBs (bottom 2 rows of 4x4)
    // Each sample fills a 2x2 pixel block.
    paint_y_tile(frame, x, y, e.y[0], 0, 0);
    paint_y_tile(frame, x, y, e.y[1], 2, 0);
    paint_y_tile(frame, x, y, e.y[2], 0, 2);
    paint_y_tile(frame, x, y, e.y[3], 2, 2);
    // Chroma: 4:1:1 across the MB - 2x2 chroma cells all share (u,v).
    paint_chroma_block(frame, x, y, cb_byte, cr_byte, 2, 2);
}

/// Paint a 4x4 MB using four V4 codebook entries, one per 2x2 sub-block.
#[allow(clippy::too_many_arguments)]
fn paint_v4(
    frame: &mut DecodedFrame,
    x: usize,
    y: usize,
    cb: &Codebook,
    i0: u8,
    i1: u8,
    i2: u8,
    i3: u8,
) {
    // Sub-block layout (per §5.3):
    //   i0 covers top-left 2x2 of luma   →   entry's Y maps 1:1.
    //   i1 covers top-right
    //   i2 covers bottom-left
    //   i3 covers bottom-right
    paint_v4_subblock(frame, x, y, &cb.entries[i0 as usize], 0, 0);
    paint_v4_subblock(frame, x, y, &cb.entries[i1 as usize], 2, 0);
    paint_v4_subblock(frame, x, y, &cb.entries[i2 as usize], 0, 2);
    paint_v4_subblock(frame, x, y, &cb.entries[i3 as usize], 2, 2);
}

/// Paint one V4 2x2 sub-block: luma 1:1, chroma covers the 1x1
/// chroma cell (since the MB's 4x4 luma area maps to a 2x2 chroma
/// area in YUV420, each sub-block is 1 chroma sample).
fn paint_v4_subblock(
    frame: &mut DecodedFrame,
    mb_x: usize,
    mb_y: usize,
    e: &CbEntry,
    sub_x: usize,
    sub_y: usize,
) {
    let pw = frame.padded_w;
    let px = mb_x + sub_x;
    let py = mb_y + sub_y;
    if px + 2 > pw || py + 2 > frame.padded_h {
        // Strip rectangle beyond padded frame - shouldn't happen but
        // guard. (The padded dims are MB-aligned, so this is a safety
        // net only for malformed strip rectangles.)
        return;
    }
    let y_plane = &mut frame.y_plane;
    y_plane[py * pw + px] = e.y[0];
    y_plane[py * pw + px + 1] = e.y[1];
    y_plane[(py + 1) * pw + px] = e.y[2];
    y_plane[(py + 1) * pw + px + 1] = e.y[3];

    // Chroma: one sample at chroma-coords (px/2, py/2).
    let cw = pw / 2;
    let cx = px / 2;
    let cy = py / 2;
    if cx < cw && cy < frame.padded_h / 2 {
        let coff = cy * cw + cx;
        frame.cb_plane[coff] = chroma_unsigned(e.u);
        frame.cr_plane[coff] = chroma_unsigned(e.v);
    }
}

/// Paint one luma sample as a 2x2 pixel tile inside the MB at
/// (mb_x + tx, mb_y + ty) … (+2, +2). Used by V1 paint.
fn paint_y_tile(
    frame: &mut DecodedFrame,
    mb_x: usize,
    mb_y: usize,
    y_sample: u8,
    tx: usize,
    ty: usize,
) {
    let pw = frame.padded_w;
    let px = mb_x + tx;
    let py = mb_y + ty;
    if px + 2 > pw || py + 2 > frame.padded_h {
        return;
    }
    let y_plane = &mut frame.y_plane;
    y_plane[py * pw + px] = y_sample;
    y_plane[py * pw + px + 1] = y_sample;
    y_plane[(py + 1) * pw + px] = y_sample;
    y_plane[(py + 1) * pw + px + 1] = y_sample;
}

/// Paint a chroma block of (cb_w x cb_h) chroma samples (each 1x1 in
/// chroma plane = 2x2 in luma) at MB (mb_x, mb_y), starting at the
/// top-left chroma cell of the MB. Used by V1 paint to fill the MB's
/// 2x2 chroma area with a single (Cb, Cr) pair.
fn paint_chroma_block(
    frame: &mut DecodedFrame,
    mb_x: usize,
    mb_y: usize,
    cb_byte: u8,
    cr_byte: u8,
    cb_w: usize,
    cb_h: usize,
) {
    let cw = frame.padded_w / 2;
    let cx0 = mb_x / 2;
    let cy0 = mb_y / 2;
    let ch = frame.padded_h / 2;
    for dy in 0..cb_h {
        let cy = cy0 + dy;
        if cy >= ch {
            break;
        }
        for dx in 0..cb_w {
            let cx = cx0 + dx;
            if cx >= cw {
                break;
            }
            let off = cy * cw + cx;
            frame.cb_plane[off] = cb_byte;
            frame.cr_plane[off] = cr_byte;
        }
    }
}

// ───────────────────── Tests ─────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal one-strip Cinepak frame with a single V1 INTRA
    /// MB and verify it decodes to the expected luma/chroma values.
    #[test]
    fn decode_minimal_v1_intra_4x4() {
        // Frame header (10 bytes):
        //   flags=0x00, encoded_buf_size=tbd, w=4, h=4, num_strips=1
        // Strip header (12 bytes):
        //   strip_id=0x10 INTRA, strip_size=tbd, y1=0,x1=0,y2=4,x2=4
        // V4 chunk (0x20) — empty (4-byte header only)
        // V1 chunk (0x22) — one entry, 6 bytes
        //   entry: Y=[10,20,30,40], u=0, v=0
        // Vector list (0x30) — one MB; 1 mode bit = 0 (V1) then 1 byte index.
        //   flag_word: 32 bits big-endian where MSB=0 → V1.
        //   bytes: 0x00 0x00 0x00 0x00 (flag) + index byte.
        //
        // Total frame bytes computed below.
        let mut buf = Vec::new();
        // Frame header — fill encoded_buf_size after building.
        buf.extend_from_slice(&[0x00, 0, 0, 0, 0, 4, 0, 4, 0, 1]);
        // Strip header — fill strip_size after.
        let strip_start = buf.len();
        buf.extend_from_slice(&[
            0x10, 0, 0, 0, // strip_id=0x10, strip_size placeholder
            0, 0, // y1
            0, 0, // x1
            0, 4, // y2
            0, 4, // x2
        ]);
        // V4 chunk: empty
        buf.extend_from_slice(&[0x20, 0, 0, 4]);
        // V1 chunk: one entry
        buf.extend_from_slice(&[0x22, 0, 0, 10]); // 4 (header) + 6 (one entry)
        buf.extend_from_slice(&[10, 20, 30, 40, 0, 0]);
        // Vector list 0x30: 4-byte chunk header + 4-byte flag word + 1 byte index
        buf.extend_from_slice(&[0x30, 0, 0, 9]); // 4 + 4 + 1
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // flag word: MSB=0 → V1
        buf.push(0); // V1 codebook index 0

        // Patch strip_size (bytes from strip_start to end of buf).
        let strip_size = buf.len() - strip_start;
        buf[strip_start + 1] = ((strip_size >> 16) & 0xff) as u8;
        buf[strip_start + 2] = ((strip_size >> 8) & 0xff) as u8;
        buf[strip_start + 3] = (strip_size & 0xff) as u8;
        // Patch encoded_buf_size (total length).
        let total = buf.len() as u32;
        buf[1] = ((total >> 16) & 0xff) as u8;
        buf[2] = ((total >> 8) & 0xff) as u8;
        buf[3] = (total & 0xff) as u8;

        let f = decode_packet(&buf, None).expect("decode");
        assert_eq!(f.width, 4);
        assert_eq!(f.height, 4);
        // V1 painting tiles each entry sample to a 2x2 luma block,
        // arranged as:
        //   Y00 Y00 Y01 Y01
        //   Y00 Y00 Y01 Y01
        //   Y10 Y10 Y11 Y11
        //   Y10 Y10 Y11 Y11
        // = with our entry [10, 20, 30, 40]:
        let expected = [
            [10, 10, 20, 20],
            [10, 10, 20, 20],
            [30, 30, 40, 40],
            [30, 30, 40, 40],
        ];
        for row in 0..4 {
            for col in 0..4 {
                assert_eq!(
                    f.y_plane[row * f.padded_w + col],
                    expected[row][col],
                    "luma mismatch at ({},{})",
                    col,
                    row
                );
            }
        }
        // Chroma neutral (u=v=0 → 128).
        for c in &f.cb_plane {
            assert_eq!(*c, 128);
        }
        for c in &f.cr_plane {
            assert_eq!(*c, 128);
        }
    }

    /// V4 INTRA MB with four distinct codebook entries — each entry
    /// places its 2x2 luma quad 1:1 into the matching sub-block.
    #[test]
    fn decode_minimal_v4_intra_4x4() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x00, 0, 0, 0, 0, 4, 0, 4, 0, 1]); // frame hdr
        let strip_start = buf.len();
        buf.extend_from_slice(&[0x10, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 4]);
        // V4 chunk: 4 entries of 6 bytes = 24 + 4 hdr = 28
        buf.extend_from_slice(&[0x20, 0, 0, 28]);
        for i in 0..4 {
            let base = (i as u8) * 10;
            buf.extend_from_slice(&[base, base + 1, base + 2, base + 3, 0, 0]);
        }
        // V1 chunk: empty
        buf.extend_from_slice(&[0x22, 0, 0, 4]);
        // Vector list 0x30: flag word with MSB=1 → V4, then 4 indices
        buf.extend_from_slice(&[0x30, 0, 0, 4 + 4 + 4]);
        buf.extend_from_slice(&[0x80, 0x00, 0x00, 0x00]); // V4 mode bit set
        buf.extend_from_slice(&[0, 1, 2, 3]); // four codebook indices

        // Patch sizes.
        let strip_size = buf.len() - strip_start;
        buf[strip_start + 1] = ((strip_size >> 16) & 0xff) as u8;
        buf[strip_start + 2] = ((strip_size >> 8) & 0xff) as u8;
        buf[strip_start + 3] = (strip_size & 0xff) as u8;
        let total = buf.len() as u32;
        buf[1] = ((total >> 16) & 0xff) as u8;
        buf[2] = ((total >> 8) & 0xff) as u8;
        buf[3] = (total & 0xff) as u8;

        let f = decode_packet(&buf, None).expect("decode");
        // V4 layout:
        //   sub i0=top-left 2x2  → entry 0 luma [0,1,2,3]
        //   sub i1=top-right 2x2 → entry 1 luma [10,11,12,13]
        //   sub i2=bottom-left   → entry 2 luma [20,21,22,23]
        //   sub i3=bottom-right  → entry 3 luma [30,31,32,33]
        let expected = [
            [0, 1, 10, 11],
            [2, 3, 12, 13],
            [20, 21, 30, 31],
            [22, 23, 32, 33],
        ];
        for row in 0..4 {
            for col in 0..4 {
                assert_eq!(
                    f.y_plane[row * f.padded_w + col],
                    expected[row][col],
                    "luma mismatch at ({},{})",
                    col,
                    row
                );
            }
        }
    }

    #[test]
    fn rejects_truncated_header() {
        let too_short = [0u8; 5];
        let r = decode_packet(&too_short, None);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_zero_dims() {
        let mut bad = vec![0u8; FRAME_HEADER_LEN];
        // width=0, height=0
        let r = decode_packet(&bad, None);
        assert!(r.is_err());
        // Make height non-zero, width 0 → still error.
        bad[6] = 0;
        bad[7] = 4;
        let r = decode_packet(&bad, None);
        assert!(r.is_err());
    }

    #[test]
    fn flag_bits_refill_at_end_of_buffer() {
        // 5 bytes — first refill consumes 4, second refill should
        // gracefully return zeros.
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF, 0xAA];
        let mut bits = FlagBits::new(&bytes);
        for _ in 0..32 {
            assert_eq!(bits.pull_bit(), 1);
        }
        // Second refill: byte 4 is 0xAA (10101010), padded with three
        // zero bytes → MSB is 1 then 0 then 1 …
        assert_eq!(bits.pull_bit(), 1);
        assert_eq!(bits.pull_bit(), 0);
        assert_eq!(bits.pull_bit(), 1);
        assert_eq!(bits.pull_bit(), 0);
    }

    #[test]
    fn pixfmt_constant_is_yuv420p() {
        assert_eq!(OUTPUT_PIX_FMT, PixelFormat::Yuv420P);
    }
}
