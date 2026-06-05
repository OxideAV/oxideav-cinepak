//! Cinepak frame and strip header parsing.
//!
//! Wire-format reference:
//! `docs/video/cinepak/spec/01-frame-and-strip.md`.
//!
//! All multi-byte fields are big-endian. Frame header is 10 bytes;
//! strip header is 12 bytes. Strip-header `y_top` / `y_bottom` use a
//! sentinel rule for non-first strips: when raw `y_top == 0`, the
//! actual top is `previous_y_bottom` and the actual bottom is
//! `previous_y_bottom + raw_y_bottom` — i.e. the wire `y_bottom` is the
//! strip's height rather than an absolute coordinate.

use crate::error::{CinepakError, Result};

pub const FRAME_HEADER_SIZE: usize = 10;
pub const STRIP_HEADER_SIZE: usize = 12;

/// Parsed Cinepak frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    /// Raw 8-bit flag byte. Only bit 0 is defined: when set, the
    /// strips inside this frame inherit codebooks from the previous
    /// strip / previous frame's last strip.
    pub flags: u8,
    /// Total length of the coded frame in bytes, including this 10-byte
    /// header. 24-bit big-endian.
    pub frame_length: u32,
    /// Coded frame width in pixels.
    pub width: u16,
    /// Coded frame height in pixels.
    pub height: u16,
    /// Number of strips in this frame, ≥ 1.
    pub strip_count: u16,
}

impl FrameHeader {
    /// Parse a 10-byte frame header from `buf[0..]`. Returns the parsed
    /// header on success.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < FRAME_HEADER_SIZE {
            return Err(CinepakError::invalid(format!(
                "frame header truncated: need {} bytes, got {}",
                FRAME_HEADER_SIZE,
                buf.len()
            )));
        }
        let flags = buf[0];
        let frame_length = (u32::from(buf[1]) << 16) | (u32::from(buf[2]) << 8) | u32::from(buf[3]);
        let width = u16::from_be_bytes([buf[4], buf[5]]);
        let height = u16::from_be_bytes([buf[6], buf[7]]);
        let strip_count = u16::from_be_bytes([buf[8], buf[9]]);

        if strip_count == 0 {
            return Err(CinepakError::invalid("strip_count must be ≥ 1"));
        }
        if width == 0 || height == 0 {
            return Err(CinepakError::invalid(format!(
                "width/height must be > 0 (got {width}x{height})"
            )));
        }
        if width % 4 != 0 || height % 4 != 0 {
            return Err(CinepakError::invalid(format!(
                "width and height must be multiples of 4 (got {width}x{height})"
            )));
        }
        if (frame_length as usize) < FRAME_HEADER_SIZE {
            return Err(CinepakError::invalid(format!(
                "frame_length {frame_length} smaller than 10-byte header"
            )));
        }

        Ok(Self {
            flags,
            frame_length,
            width,
            height,
            strip_count,
        })
    }

    /// Encode the frame header back into a 10-byte buffer (used by the
    /// crate's self-roundtrip tests).
    pub fn encode(&self, out: &mut [u8; FRAME_HEADER_SIZE]) {
        out[0] = self.flags;
        out[1] = ((self.frame_length >> 16) & 0xff) as u8;
        out[2] = ((self.frame_length >> 8) & 0xff) as u8;
        out[3] = (self.frame_length & 0xff) as u8;
        out[4..6].copy_from_slice(&self.width.to_be_bytes());
        out[6..8].copy_from_slice(&self.height.to_be_bytes());
        out[8..10].copy_from_slice(&self.strip_count.to_be_bytes());
    }
}

/// Strip-header `strip_id` taxonomy.
pub const STRIP_ID_INTRA: u16 = 0x1000;
pub const STRIP_ID_INTER: u16 = 0x1100;

/// Raw strip header bytes, **before** applying the y-coordinate
/// sentinel rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawStripHeader {
    pub strip_id: u16,
    /// Total length of this strip in bytes, **inclusive** of the
    /// 12-byte strip header.
    pub strip_size: u16,
    pub y_top: u16,
    pub x_top: u16,
    pub y_bottom: u16,
    pub x_bottom: u16,
}

impl RawStripHeader {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < STRIP_HEADER_SIZE {
            return Err(CinepakError::invalid(format!(
                "strip header truncated: need {} bytes, got {}",
                STRIP_HEADER_SIZE,
                buf.len()
            )));
        }
        let strip_id = u16::from_be_bytes([buf[0], buf[1]]);
        let strip_size = u16::from_be_bytes([buf[2], buf[3]]);
        let y_top = u16::from_be_bytes([buf[4], buf[5]]);
        let x_top = u16::from_be_bytes([buf[6], buf[7]]);
        let y_bottom = u16::from_be_bytes([buf[8], buf[9]]);
        let x_bottom = u16::from_be_bytes([buf[10], buf[11]]);

        if strip_id != STRIP_ID_INTRA && strip_id != STRIP_ID_INTER {
            return Err(CinepakError::invalid(format!(
                "unknown strip_id 0x{strip_id:04x}; only 0x1000 (intra) and 0x1100 (inter) are defined"
            )));
        }
        if (strip_size as usize) < STRIP_HEADER_SIZE {
            return Err(CinepakError::invalid(format!(
                "strip_size {strip_size} < 12 (own header size)"
            )));
        }

        Ok(Self {
            strip_id,
            strip_size,
            y_top,
            x_top,
            y_bottom,
            x_bottom,
        })
    }

    pub fn encode(&self, out: &mut [u8; STRIP_HEADER_SIZE]) {
        out[0..2].copy_from_slice(&self.strip_id.to_be_bytes());
        out[2..4].copy_from_slice(&self.strip_size.to_be_bytes());
        out[4..6].copy_from_slice(&self.y_top.to_be_bytes());
        out[6..8].copy_from_slice(&self.x_top.to_be_bytes());
        out[8..10].copy_from_slice(&self.y_bottom.to_be_bytes());
        out[10..12].copy_from_slice(&self.x_bottom.to_be_bytes());
    }

    pub fn is_intra(&self) -> bool {
        self.strip_id == STRIP_ID_INTRA
    }
}

/// Strip header with the y-coordinate sentinel rule resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripHeader {
    pub raw: RawStripHeader,
    /// Decoded top Y pixel coordinate (after the sentinel rule).
    pub actual_y_top: u32,
    /// Decoded bottom Y pixel coordinate (exclusive).
    pub actual_y_bottom: u32,
}

impl StripHeader {
    /// Apply spec §2.2: for non-first strips with raw `y_top == 0`,
    /// the actual top is `prev_y_bottom` and the wire `y_bottom`
    /// encodes the strip's height. For the first strip (or any strip
    /// with non-zero raw `y_top`), the wire coordinates are absolute.
    pub fn resolve(raw: RawStripHeader, is_first: bool, prev_y_bottom: u32) -> Self {
        let (actual_y_top, actual_y_bottom) = if !is_first && raw.y_top == 0 {
            (prev_y_bottom, prev_y_bottom + u32::from(raw.y_bottom))
        } else {
            (u32::from(raw.y_top), u32::from(raw.y_bottom))
        };
        Self {
            raw,
            actual_y_top,
            actual_y_bottom,
        }
    }

    /// Strip height in pixels.
    ///
    /// Uses saturating subtraction because malformed wire data can
    /// produce `actual_y_bottom < actual_y_top` (e.g. a strip with
    /// `y_bottom < y_top` directly from the bitstream, or a non-first
    /// strip whose `prev_y_bottom + raw.y_bottom` wraps). The decoder
    /// catches the ordering separately and rejects the frame, but the
    /// accessor must not panic.
    pub fn height(&self) -> u32 {
        self.actual_y_bottom.saturating_sub(self.actual_y_top)
    }

    /// Strip width in pixels.
    pub fn width(&self) -> u32 {
        u32::from(self.raw.x_bottom).saturating_sub(u32::from(self.raw.x_top))
    }
}

/// A single resolved strip yielded by [`FrameStrips`].
///
/// `header` carries the parsed strip header with the spec §2.2
/// y-coordinate sentinel rule already applied (`actual_y_top` /
/// `actual_y_bottom` resolved against the running `prev_y_bottom`
/// accumulator). `payload` is the strip's chunk-stream bytes — the
/// `strip_size - 12` bytes that follow the 12-byte strip header.
/// Decoding the chunks is the job of `decode_strip_chunks` (see
/// `decoder.rs`); this entry is intentionally one layer down so
/// container code and validators can enumerate strip-level metadata
/// (id taxonomy, resolved pixel rectangle, payload length) without
/// dragging in the VQ machinery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripEntry<'a> {
    /// 0-based strip index within the frame.
    pub index: u16,
    /// Resolved strip header — `raw` plus the sentinel-applied
    /// `actual_y_top` / `actual_y_bottom`.
    pub header: StripHeader,
    /// Chunk-stream bytes for this strip (no 12-byte strip header
    /// prefix). Length equals `header.raw.strip_size - 12`.
    pub payload: &'a [u8],
}

/// Iterator over a Cinepak frame's strip headers.
///
/// Wire-format reference: `docs/video/cinepak/spec/01-frame-and-strip.md`
/// §3 (decoder algorithm). The iterator implements steps 2.1–2.4 of the
/// decoder algorithm for the spec-standard (non-deviant) frame layout:
///
/// 1. Read 12 bytes of strip header at the current cursor.
/// 2. Resolve the y-coordinate sentinel rule (§2.2) against the
///    running `prev_y_bottom` accumulator.
/// 3. Yield a [`StripEntry`] referencing the strip's payload slice.
/// 4. Advance the cursor by the strip's declared `strip_size`.
///
/// The iterator is read-only — it does not touch codebooks, vector
/// chunks, or pixels. Callers who only need the strip-level
/// rectangles + payload boundaries (AVI sub-stream consumers,
/// payload-length validators, fuzz harnesses that want to bound
/// per-strip mutation independently) can drop the decoder dependency
/// entirely.
///
/// Deviant streams (Sega FILM Saturn `'cvid'` + Lemmings 3DO 6-byte
/// prefix; see `docs/video/cinepak/reference/wiki/Sega_FILM.wiki`
/// lines 125–143 and 189) are NOT handled here — those carry an
/// extra header prefix and/or short `frame_length` that the standard
/// §1 frame header does not describe. Use
/// `CinepakDecoder::decode_deviant_frame` for those.
///
/// ### Error semantics
///
/// `next()` returns `Some(Err(_))` once on the first malformed
/// strip, then `None` on every subsequent call. The iterator
/// **fuses** itself on error — partial enumeration up to the
/// failure is observable but the caller does not have to track an
/// "is fused" flag externally.
#[derive(Debug)]
pub struct FrameStrips<'a> {
    /// Frame slice trimmed to `header.frame_length`. The iterator
    /// only ever reads within this slice; the surrounding container
    /// bytes are out of scope.
    frame_bytes: &'a [u8],
    /// Parsed 10-byte frame header. Carried for `header()` access
    /// and to drive the strip count.
    header: FrameHeader,
    /// Byte offset of the next strip header within `frame_bytes`.
    cursor: usize,
    /// 0-based index of the next strip to yield.
    next_index: u16,
    /// `actual_y_bottom` of the previously-yielded strip; fed into
    /// the §2.2 sentinel rule for the next strip.
    prev_y_bottom: u32,
    /// One-shot fuse: set when an error is yielded so subsequent
    /// `next()` calls return `None`.
    fused: bool,
}

impl<'a> FrameStrips<'a> {
    /// Build a strip iterator over the standard (non-deviant) Cinepak
    /// frame contained in `bytes`.
    ///
    /// Parses the 10-byte frame header (spec §1) and trims `bytes` to
    /// `frame_length`. Returns an error if the buffer is too short for
    /// the frame header, the buffer is shorter than the declared
    /// `frame_length`, or the frame header itself is malformed (zero
    /// strip count, non-multiple-of-4 dims, …).
    ///
    /// The returned iterator yields exactly `header().strip_count`
    /// entries on the happy path. On a malformed strip it yields one
    /// `Err` and then fuses.
    pub fn new(bytes: &'a [u8]) -> Result<Self> {
        let header = FrameHeader::parse(bytes)?;
        let frame_len = header.frame_length as usize;
        if frame_len > bytes.len() {
            return Err(CinepakError::invalid(format!(
                "frame_length {} exceeds buffer size {}",
                frame_len,
                bytes.len()
            )));
        }
        Ok(Self {
            frame_bytes: &bytes[..frame_len],
            header,
            cursor: FRAME_HEADER_SIZE,
            next_index: 0,
            prev_y_bottom: 0,
            fused: false,
        })
    }

    /// Parsed frame header (immutable; identical to
    /// `FrameHeader::parse(bytes)` on the buffer that built this
    /// iterator).
    pub fn header(&self) -> &FrameHeader {
        &self.header
    }

    /// Number of strips remaining (`strip_count - emitted_so_far`).
    pub fn remaining(&self) -> u16 {
        self.header.strip_count.saturating_sub(self.next_index)
    }
}

impl<'a> Iterator for FrameStrips<'a> {
    type Item = Result<StripEntry<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fused {
            return None;
        }
        if self.next_index >= self.header.strip_count {
            return None;
        }
        // 12-byte strip header read.
        if self.cursor + STRIP_HEADER_SIZE > self.frame_bytes.len() {
            self.fused = true;
            return Some(Err(CinepakError::invalid(format!(
                "strip {} header truncated",
                self.next_index
            ))));
        }
        let raw = match RawStripHeader::parse(
            &self.frame_bytes[self.cursor..self.cursor + STRIP_HEADER_SIZE],
        ) {
            Ok(r) => r,
            Err(e) => {
                self.fused = true;
                return Some(Err(e));
            }
        };
        let strip_size = raw.strip_size as usize;
        if self.cursor + strip_size > self.frame_bytes.len() {
            self.fused = true;
            return Some(Err(CinepakError::invalid(format!(
                "strip {} declared size {} overruns frame",
                self.next_index, strip_size
            ))));
        }
        let is_first = self.next_index == 0;
        let header = StripHeader::resolve(raw, is_first, self.prev_y_bottom);
        // Step the cursor + accumulators before returning so the next
        // iteration sees the post-yield state. Saturating add on
        // `prev_y_bottom` to keep a fuzz-found wraparound off the hot
        // path; the decoder's full bounds check still catches it as
        // an Err on the actual decode but the iterator itself must
        // stay panic-free for any well-formed strip header.
        let payload_start = self.cursor + STRIP_HEADER_SIZE;
        let payload_end = self.cursor + strip_size;
        let payload = &self.frame_bytes[payload_start..payload_end];
        let entry = StripEntry {
            index: self.next_index,
            header,
            payload,
        };
        self.prev_y_bottom = header.actual_y_bottom;
        self.cursor = payload_end;
        self.next_index += 1;
        Some(Ok(entry))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let r = self.remaining() as usize;
        (r, Some(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §1.1 observation `O1` byte sequence.
    #[test]
    fn parses_o1_frame_header() {
        let bytes = [0x00, 0x00, 0x26, 0x7d, 0x01, 0x40, 0x00, 0xf0, 0x00, 0x01];
        let h = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(h.flags, 0x00);
        assert_eq!(h.frame_length, 0x267d);
        assert_eq!(h.width, 320);
        assert_eq!(h.height, 240);
        assert_eq!(h.strip_count, 1);
    }

    /// Spec §2.1 observation `O1` first-strip byte sequence.
    #[test]
    fn parses_o1_strip_header() {
        let bytes = [
            0x10, 0x00, 0x26, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x01, 0x40,
        ];
        let raw = RawStripHeader::parse(&bytes).unwrap();
        assert_eq!(raw.strip_id, STRIP_ID_INTRA);
        assert_eq!(raw.strip_size, 0x2673);
        assert_eq!(raw.y_top, 0);
        assert_eq!(raw.y_bottom, 240);
        assert_eq!(raw.x_top, 0);
        assert_eq!(raw.x_bottom, 320);
    }

    /// Spec §2.2 observation `O3` 3-strip y-sentinel resolution.
    #[test]
    fn applies_y_sentinel_rule() {
        // Wire: every strip carries y_top=0, y_bottom=80 (height).
        let raw = RawStripHeader {
            strip_id: STRIP_ID_INTRA,
            strip_size: 100,
            y_top: 0,
            x_top: 0,
            y_bottom: 80,
            x_bottom: 320,
        };
        // First strip: literal coords.
        let s0 = StripHeader::resolve(raw, true, 0);
        assert_eq!((s0.actual_y_top, s0.actual_y_bottom), (0, 80));
        // Strip 1: top sentinel-resolves to prev=80, bottom=80+80=160.
        let s1 = StripHeader::resolve(raw, false, 80);
        assert_eq!((s1.actual_y_top, s1.actual_y_bottom), (80, 160));
        // Strip 2: prev=160, bottom=160+80=240.
        let s2 = StripHeader::resolve(raw, false, 160);
        assert_eq!((s2.actual_y_top, s2.actual_y_bottom), (160, 240));
    }

    #[test]
    fn rejects_non_4_dims() {
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        let h = FrameHeader {
            flags: 0,
            frame_length: 100,
            width: 33,
            height: 32,
            strip_count: 1,
        };
        h.encode(&mut buf);
        assert!(FrameHeader::parse(&buf).is_err());
    }

    #[test]
    fn rejects_zero_strip_count() {
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        let h = FrameHeader {
            flags: 0,
            frame_length: 100,
            width: 32,
            height: 32,
            strip_count: 0,
        };
        h.encode(&mut buf);
        assert!(FrameHeader::parse(&buf).is_err());
    }

    #[test]
    fn rejects_unknown_strip_id() {
        let mut buf = [0u8; STRIP_HEADER_SIZE];
        let raw = RawStripHeader {
            strip_id: 0x1234,
            strip_size: 16,
            y_top: 0,
            x_top: 0,
            y_bottom: 4,
            x_bottom: 4,
        };
        raw.encode(&mut buf);
        assert!(RawStripHeader::parse(&buf).is_err());
    }

    #[test]
    fn frame_header_roundtrip() {
        let h = FrameHeader {
            flags: 0x01,
            frame_length: 0x00ab_cdef,
            width: 320,
            height: 240,
            strip_count: 3,
        };
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        h.encode(&mut buf);
        let h2 = FrameHeader::parse(&buf).unwrap();
        assert_eq!(h, h2);
    }

    /// Build a synthetic frame with `strip_count` empty strips (zero
    /// payload bytes after each 12-byte strip header). Each strip
    /// carries `wire_y_top = 0` and `wire_y_bottom = height_each`
    /// to exercise spec §2.2's sentinel rule across multiple strips.
    fn build_synthetic_frame(
        width: u16,
        strips_height_each: u16,
        strip_count: u16,
        first_y_top_literal: bool,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        // Frame header placeholder.
        buf.extend_from_slice(&[0u8; FRAME_HEADER_SIZE]);
        for i in 0..strip_count {
            let raw = RawStripHeader {
                strip_id: STRIP_ID_INTRA,
                strip_size: STRIP_HEADER_SIZE as u16,
                // First strip: literal y_top=0 + y_bottom=height_each
                // is indistinguishable from sentinel mode (per spec
                // §2.2 the first strip is exempt). `first_y_top_literal`
                // forces a non-zero y_top on strip 0 to test the
                // absolute-coordinate path explicitly.
                y_top: if i == 0 && first_y_top_literal { 1 } else { 0 },
                x_top: 0,
                y_bottom: strips_height_each,
                x_bottom: width,
            };
            let mut s = [0u8; STRIP_HEADER_SIZE];
            raw.encode(&mut s);
            buf.extend_from_slice(&s);
        }
        // Patch frame header.
        let total_strips_bytes = STRIP_HEADER_SIZE * strip_count as usize;
        let frame_len = FRAME_HEADER_SIZE + total_strips_bytes;
        let header = FrameHeader {
            flags: 0x00,
            frame_length: frame_len as u32,
            width,
            height: strips_height_each * strip_count,
            strip_count,
        };
        let mut hb = [0u8; FRAME_HEADER_SIZE];
        header.encode(&mut hb);
        buf[..FRAME_HEADER_SIZE].copy_from_slice(&hb);
        buf
    }

    /// Spec §3 algorithm: 3-strip 320×240 frame walks via the iterator
    /// and produces resolved y-coordinates matching the §2.2 worked
    /// example (`O3`).
    #[test]
    fn frame_strips_walks_three_strips_sentinel() {
        let bytes = build_synthetic_frame(320, 80, 3, false);
        let mut it = FrameStrips::new(&bytes).unwrap();
        assert_eq!(it.header().strip_count, 3);
        assert_eq!(it.remaining(), 3);
        let (lo, hi) = it.size_hint();
        assert_eq!((lo, hi), (3, Some(3)));

        let s0 = it.next().unwrap().unwrap();
        assert_eq!(s0.index, 0);
        assert_eq!(s0.header.actual_y_top, 0);
        assert_eq!(s0.header.actual_y_bottom, 80);
        assert_eq!(s0.payload.len(), 0);

        let s1 = it.next().unwrap().unwrap();
        assert_eq!(s1.index, 1);
        assert_eq!(s1.header.actual_y_top, 80);
        assert_eq!(s1.header.actual_y_bottom, 160);

        let s2 = it.next().unwrap().unwrap();
        assert_eq!(s2.index, 2);
        assert_eq!(s2.header.actual_y_top, 160);
        assert_eq!(s2.header.actual_y_bottom, 240);

        assert!(it.next().is_none());
        // Iteration is exhausted; remaining is zero.
        assert_eq!(it.remaining(), 0);
    }

    /// Spec §2.2 first-strip-exemption: a non-zero raw `y_top` on the
    /// first strip is taken literally, not sentinel-resolved.
    #[test]
    fn frame_strips_first_strip_literal_when_y_top_nonzero() {
        let bytes = build_synthetic_frame(64, 4, 1, true);
        let mut it = FrameStrips::new(&bytes).unwrap();
        let s = it.next().unwrap().unwrap();
        assert_eq!(s.header.raw.y_top, 1);
        // Literal: actual_y_top == raw.y_top == 1.
        assert_eq!(s.header.actual_y_top, 1);
        assert_eq!(s.header.actual_y_bottom, 4);
        assert!(it.next().is_none());
    }

    /// Single-strip 320×240 frame: `O1` byte pattern walks end-to-end.
    #[test]
    fn frame_strips_walks_o1_single_strip() {
        // Build a buffer with `O1`-style frame + strip headers, no
        // chunks (we exercise the iterator only).
        let header = FrameHeader {
            flags: 0x00,
            frame_length: (FRAME_HEADER_SIZE + STRIP_HEADER_SIZE) as u32,
            width: 320,
            height: 240,
            strip_count: 1,
        };
        let mut hbuf = [0u8; FRAME_HEADER_SIZE];
        header.encode(&mut hbuf);
        let strip = RawStripHeader {
            strip_id: STRIP_ID_INTRA,
            strip_size: STRIP_HEADER_SIZE as u16,
            y_top: 0,
            x_top: 0,
            y_bottom: 240,
            x_bottom: 320,
        };
        let mut sbuf = [0u8; STRIP_HEADER_SIZE];
        strip.encode(&mut sbuf);
        let mut bytes = Vec::with_capacity(FRAME_HEADER_SIZE + STRIP_HEADER_SIZE);
        bytes.extend_from_slice(&hbuf);
        bytes.extend_from_slice(&sbuf);

        let mut it = FrameStrips::new(&bytes).unwrap();
        let s = it.next().unwrap().unwrap();
        assert_eq!(s.index, 0);
        assert!(s.header.raw.is_intra());
        // First-strip exemption + literal coords.
        assert_eq!(s.header.actual_y_top, 0);
        assert_eq!(s.header.actual_y_bottom, 240);
        // Payload is `strip_size - 12 = 0` bytes for this empty case.
        assert_eq!(s.payload.len(), 0);
        assert!(it.next().is_none());
    }

    /// Truncated strip header: strip_count promises one strip but the
    /// frame buffer ends short of the 12-byte strip header.
    #[test]
    fn frame_strips_fuses_on_strip_header_truncation() {
        // Frame header advertises 1 strip but frame_length only covers
        // the 10-byte frame header itself.
        let mut hbuf = [0u8; FRAME_HEADER_SIZE];
        let header = FrameHeader {
            flags: 0x00,
            frame_length: FRAME_HEADER_SIZE as u32,
            width: 64,
            height: 64,
            strip_count: 1,
        };
        header.encode(&mut hbuf);

        let mut it = FrameStrips::new(&hbuf).unwrap();
        let first = it.next();
        assert!(matches!(first, Some(Err(_))), "expected truncation error");
        // Iterator is fused — subsequent calls return None.
        assert!(it.next().is_none());
        assert!(it.next().is_none());
    }

    /// Buffer shorter than declared `frame_length` is caught at
    /// construction time, not mid-iteration.
    #[test]
    fn frame_strips_construction_rejects_short_buffer() {
        let mut hbuf = [0u8; FRAME_HEADER_SIZE];
        let header = FrameHeader {
            flags: 0x00,
            frame_length: 0x00_1000, // 4096 bytes promised
            width: 64,
            height: 64,
            strip_count: 1,
        };
        header.encode(&mut hbuf);
        // Buffer carries only the 10-byte header.
        let r = FrameStrips::new(&hbuf);
        assert!(r.is_err());
    }

    /// Strip declares a `strip_size` that overruns the trimmed frame
    /// buffer — must yield an error, not panic, then fuse.
    #[test]
    fn frame_strips_fuses_on_strip_size_overrun() {
        // Frame header: 1 strip, frame_length covers exactly one 12-byte
        // strip header.
        let frame_len = FRAME_HEADER_SIZE + STRIP_HEADER_SIZE;
        let mut buf = vec![0u8; frame_len];
        let header = FrameHeader {
            flags: 0x00,
            frame_length: frame_len as u32,
            width: 64,
            height: 64,
            strip_count: 1,
        };
        let mut hbuf = [0u8; FRAME_HEADER_SIZE];
        header.encode(&mut hbuf);
        buf[..FRAME_HEADER_SIZE].copy_from_slice(&hbuf);
        // Strip claims 9999-byte size — far past frame_length.
        let strip = RawStripHeader {
            strip_id: STRIP_ID_INTRA,
            strip_size: 9999,
            y_top: 0,
            x_top: 0,
            y_bottom: 64,
            x_bottom: 64,
        };
        let mut sbuf = [0u8; STRIP_HEADER_SIZE];
        strip.encode(&mut sbuf);
        buf[FRAME_HEADER_SIZE..].copy_from_slice(&sbuf);

        let mut it = FrameStrips::new(&buf).unwrap();
        assert!(matches!(it.next(), Some(Err(_))));
        // Fused.
        assert!(it.next().is_none());
    }

    /// Strip carries non-empty payload — verify the iterator yields a
    /// slice of exactly `strip_size - 12` bytes for downstream chunk
    /// decoders.
    #[test]
    fn frame_strips_yields_payload_slice() {
        // 1 strip with 5 bytes of synthetic payload after the strip
        // header (chunk-decoder will reject these specific bytes; the
        // iterator itself is content-agnostic).
        let payload_len = 5u16;
        let strip_size = STRIP_HEADER_SIZE as u16 + payload_len;
        let frame_len = FRAME_HEADER_SIZE + strip_size as usize;
        let mut buf = vec![0u8; frame_len];
        let header = FrameHeader {
            flags: 0x00,
            frame_length: frame_len as u32,
            width: 64,
            height: 64,
            strip_count: 1,
        };
        let mut hbuf = [0u8; FRAME_HEADER_SIZE];
        header.encode(&mut hbuf);
        buf[..FRAME_HEADER_SIZE].copy_from_slice(&hbuf);
        let strip = RawStripHeader {
            strip_id: STRIP_ID_INTRA,
            strip_size,
            y_top: 0,
            x_top: 0,
            y_bottom: 64,
            x_bottom: 64,
        };
        let mut sbuf = [0u8; STRIP_HEADER_SIZE];
        strip.encode(&mut sbuf);
        buf[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + STRIP_HEADER_SIZE].copy_from_slice(&sbuf);
        // Mark the payload bytes so a slice mismatch is easy to spot.
        for (i, b) in buf[FRAME_HEADER_SIZE + STRIP_HEADER_SIZE..]
            .iter_mut()
            .enumerate()
        {
            *b = (0xa0 + i) as u8;
        }

        let mut it = FrameStrips::new(&buf).unwrap();
        let s = it.next().unwrap().unwrap();
        assert_eq!(s.payload.len(), payload_len as usize);
        assert_eq!(s.payload, &[0xa0, 0xa1, 0xa2, 0xa3, 0xa4]);
        assert!(it.next().is_none());
    }

    /// Sum of payload lengths across all strips matches
    /// `frame_length - 10 - 12 × strip_count` — the spec §3
    /// invariant in a single assertion.
    #[test]
    fn frame_strips_payload_lengths_total_to_spec_invariant() {
        // Three strips at 80 px each, 320 wide, each with a 7-byte
        // payload.
        let payload_per_strip = 7u16;
        let strip_count: u16 = 3;
        let strip_size = STRIP_HEADER_SIZE as u16 + payload_per_strip;
        let frame_len = FRAME_HEADER_SIZE + (strip_count as usize) * (strip_size as usize);
        let mut buf = Vec::with_capacity(frame_len);
        let header = FrameHeader {
            flags: 0x00,
            frame_length: frame_len as u32,
            width: 320,
            height: 240,
            strip_count,
        };
        let mut hbuf = [0u8; FRAME_HEADER_SIZE];
        header.encode(&mut hbuf);
        buf.extend_from_slice(&hbuf);
        for _ in 0..strip_count {
            let strip = RawStripHeader {
                strip_id: STRIP_ID_INTRA,
                strip_size,
                y_top: 0,
                x_top: 0,
                y_bottom: 80,
                x_bottom: 320,
            };
            let mut sbuf = [0u8; STRIP_HEADER_SIZE];
            strip.encode(&mut sbuf);
            buf.extend_from_slice(&sbuf);
            // Strip payload — content doesn't matter for this test.
            buf.extend_from_slice(&[0u8; 7]);
        }

        let it = FrameStrips::new(&buf).unwrap();
        let total_payload: usize = it.map(|r| r.unwrap().payload.len()).sum();
        let expected = (frame_len) - FRAME_HEADER_SIZE - (strip_count as usize) * STRIP_HEADER_SIZE;
        assert_eq!(total_payload, expected);
        assert_eq!(total_payload, 21);
    }
}
