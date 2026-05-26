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
}
