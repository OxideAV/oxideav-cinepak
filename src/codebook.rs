//! Cinepak codebook chunks and entry layout.
//!
//! Wire-format reference:
//! `docs/video/cinepak/spec/02-codebooks.md`. Codebook chunks live
//! inside a strip's chunk stream after the 12-byte strip header and
//! before the strip's vector chunk.
//!
//! Each codebook chunk is 4 bytes of header (`chunk_id` + `chunk_size`)
//! followed by a payload that defines or updates entries in either the
//! V4 or V1 codebook. The chunk-id high byte's low nibble is a 3-bit
//! selector (§2.1):
//!
//! - bit 2 (`0x0400`): clear = 12-bit YUV (6-byte entries); set = 8-bit
//!   grayscale (4-byte entries).
//! - bit 1 (`0x0200`): clear = V4 codebook; set = V1 codebook.
//! - bit 0 (`0x0100`): clear = full replacement; set = selective update.

use crate::error::{CinepakError, Result};

/// Common 4-byte chunk header (chunk-id + chunk-size).
pub const CHUNK_HEADER_SIZE: usize = 4;

/// One codebook entry. The decoder stores all entries in 12-bit-YUV
/// shape internally; for grayscale streams the chroma fields are zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodebookEntry {
    pub y0: u8,
    pub y1: u8,
    pub y2: u8,
    pub y3: u8,
    /// Signed 8-bit two's-complement chroma (centred at zero); zero for
    /// grayscale streams.
    pub u: i8,
    /// Signed 8-bit two's-complement chroma; zero for grayscale.
    pub v: i8,
}

impl CodebookEntry {
    pub fn from_yuv(y0: u8, y1: u8, y2: u8, y3: u8, u: i8, v: i8) -> Self {
        Self {
            y0,
            y1,
            y2,
            y3,
            u,
            v,
        }
    }

    pub fn from_y(y0: u8, y1: u8, y2: u8, y3: u8) -> Self {
        Self {
            y0,
            y1,
            y2,
            y3,
            u: 0,
            v: 0,
        }
    }
}

/// 256-entry codebook (V4 or V1).
#[derive(Clone, Debug)]
pub struct Codebook {
    pub entries: [CodebookEntry; 256],
}

impl Default for Codebook {
    fn default() -> Self {
        Self {
            entries: [CodebookEntry::default(); 256],
        }
    }
}

/// The strip's pixel mode: 12-bit YUV (6-byte entries) or 8-bit
/// grayscale (4-byte entries). Cinepak frames are uniform per-frame:
/// one mode per stream / per frame, signalled per chunk by the
/// chunk-id high byte's bit-2 selector (spec §4 of 04-yuv-rgb-matrix.md
/// and §2.1 of 02-codebooks.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelMode {
    /// 12-bit YUV — 6-byte entries.
    Yuv12,
    /// 8-bit grayscale — 4-byte entries.
    Gray8,
}

impl PixelMode {
    pub fn entry_size(self) -> usize {
        match self {
            PixelMode::Yuv12 => 6,
            PixelMode::Gray8 => 4,
        }
    }

    /// Promote with another mode observed in the same frame; returns
    /// an error if mixed (spec §4 of 04: "A conformant stream MUST NOT
    /// mix the two flavours within the same frame").
    pub fn unify(self, other: PixelMode) -> Result<PixelMode> {
        if self != other {
            Err(CinepakError::invalid(format!(
                "mixed pixel modes in single frame ({self:?} and {other:?})"
            )))
        } else {
            Ok(self)
        }
    }
}

/// Which codebook (V4 or V1) the chunk addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhichCodebook {
    V4,
    V1,
}

/// Whether the chunk replaces the codebook from index 0 (full) or
/// merges into the previous codebook (selective update).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStyle {
    Full,
    Selective,
}

/// A decoded codebook-chunk-id, classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodebookChunkKind {
    pub which: WhichCodebook,
    pub style: UpdateStyle,
    pub mode: PixelMode,
}

impl CodebookChunkKind {
    /// Recognise a 16-bit big-endian chunk-id; returns `None` if the
    /// id doesn't belong to the codebook chunk family (`0x20xx` /
    /// `0x21xx` / `0x22xx` / `0x23xx` / `0x24xx` / `0x25xx` / `0x26xx`
    /// / `0x27xx`).
    pub fn from_id(id: u16) -> Option<Self> {
        if id & 0xff00 < 0x2000 || id & 0xff00 > 0x2700 {
            return None;
        }
        if id & 0x00ff != 0 {
            // Low byte must be 0x00 in conformant streams (spec §2.1).
            return None;
        }
        let high = (id >> 8) as u8;
        // bits 0..3 of the high byte are the selector.
        let selector = high & 0x07;
        let mode = if selector & 0b100 != 0 {
            PixelMode::Gray8
        } else {
            PixelMode::Yuv12
        };
        let which = if selector & 0b010 != 0 {
            WhichCodebook::V1
        } else {
            WhichCodebook::V4
        };
        let style = if selector & 0b001 != 0 {
            UpdateStyle::Selective
        } else {
            UpdateStyle::Full
        };
        Some(Self { which, style, mode })
    }

    pub fn to_id(self) -> u16 {
        let mut high = 0x20u8;
        if self.mode == PixelMode::Gray8 {
            high |= 0b100;
        }
        if self.which == WhichCodebook::V1 {
            high |= 0b010;
        }
        if self.style == UpdateStyle::Selective {
            high |= 0b001;
        }
        u16::from(high) << 8
    }
}

/// Decode a codebook chunk's payload into the supplied codebook.
///
/// `chunk_id` is the 16-bit chunk type; `payload` is the chunk's bytes
/// **excluding** the 4-byte chunk header. `cb` is mutated in place.
///
/// On full-replacement chunks, entries `0..N-1` are set, where `N` is
/// the chunk's entry count; on selective-update chunks, entries
/// addressed by set bits in each group's flag word are replaced and the
/// rest remain unchanged.
pub fn apply_codebook_chunk(
    kind: CodebookChunkKind,
    payload: &[u8],
    cb: &mut Codebook,
) -> Result<()> {
    apply_codebook_chunk_with(kind, payload, cb, false)
}

/// Codebook chunk decode with a `tolerate_trailing` knob for the Sega
/// Saturn deviant CVID variant.
///
/// In Sega Saturn FILM files (per `Sega_FILM.wiki` line 143), a 0x2000
/// chunk may declare a payload size that is **not** a clean multiple of
/// the 6-byte entry stride — e.g. an 0x5FC-byte payload that decodes to
/// 255 entries plus 2 padding bytes the decoder must skip before
/// continuing with the next chunk. When `tolerate_trailing` is `true`,
/// full-replacement payloads are truncated to `floor(len / entry_size)`
/// entries and the remainder is silently discarded. Standard streams
/// (the default for `apply_codebook_chunk`) are stricter and reject any
/// remainder.
pub fn apply_codebook_chunk_with(
    kind: CodebookChunkKind,
    payload: &[u8],
    cb: &mut Codebook,
    tolerate_trailing: bool,
) -> Result<()> {
    let entry_size = kind.mode.entry_size();
    match kind.style {
        UpdateStyle::Full => apply_full(kind, entry_size, payload, cb, tolerate_trailing),
        UpdateStyle::Selective => apply_selective(kind, entry_size, payload, cb),
    }
}

fn read_entry(mode: PixelMode, bytes: &[u8]) -> CodebookEntry {
    match mode {
        PixelMode::Yuv12 => CodebookEntry {
            y0: bytes[0],
            y1: bytes[1],
            y2: bytes[2],
            y3: bytes[3],
            u: bytes[4] as i8,
            v: bytes[5] as i8,
        },
        PixelMode::Gray8 => CodebookEntry {
            y0: bytes[0],
            y1: bytes[1],
            y2: bytes[2],
            y3: bytes[3],
            u: 0,
            v: 0,
        },
    }
}

fn write_entry(mode: PixelMode, e: &CodebookEntry, out: &mut [u8]) {
    out[0] = e.y0;
    out[1] = e.y1;
    out[2] = e.y2;
    out[3] = e.y3;
    if let PixelMode::Yuv12 = mode {
        out[4] = e.u as u8;
        out[5] = e.v as u8;
    }
}

fn apply_full(
    _kind: CodebookChunkKind,
    entry_size: usize,
    payload: &[u8],
    cb: &mut Codebook,
    tolerate_trailing: bool,
) -> Result<()> {
    if payload.len() % entry_size != 0 && !tolerate_trailing {
        return Err(CinepakError::invalid(format!(
            "full-replace codebook payload size {} not a multiple of entry size {entry_size}",
            payload.len()
        )));
    }
    // In the Saturn deviant variant, the payload may include 1..entry_size
    // trailing bytes that the decoder is expected to truncate. We pick
    // the floor and discard the remainder.
    let n = payload.len() / entry_size;
    if n > 256 {
        return Err(CinepakError::invalid(format!(
            "full-replace codebook has {n} entries (> 256 max)"
        )));
    }
    for i in 0..n {
        let off = i * entry_size;
        cb.entries[i] = read_entry(_kind.mode, &payload[off..off + entry_size]);
    }
    Ok(())
}

fn apply_selective(
    _kind: CodebookChunkKind,
    entry_size: usize,
    payload: &[u8],
    cb: &mut Codebook,
) -> Result<()> {
    let mut p = 0;
    let mut slot_base = 0usize;
    while p < payload.len() {
        if slot_base >= 256 {
            return Err(CinepakError::invalid(
                "selective-update codebook overruns 256 slots",
            ));
        }
        if payload.len() - p < 4 {
            return Err(CinepakError::invalid(
                "selective-update codebook truncated mid-flag-word",
            ));
        }
        let flag = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]]);
        p += 4;
        // 32 slots per group; bit 31 (MSB) selects slot 0 of the group.
        for bit in 0..32 {
            let slot = slot_base + bit;
            if slot >= 256 {
                break;
            }
            let mask = 1u32 << (31 - bit);
            if flag & mask != 0 {
                if payload.len() - p < entry_size {
                    return Err(CinepakError::invalid(
                        "selective-update codebook truncated mid-entry",
                    ));
                }
                cb.entries[slot] = read_entry(_kind.mode, &payload[p..p + entry_size]);
                p += entry_size;
            }
        }
        slot_base += 32;
    }
    Ok(())
}

/// Encode a full-replacement codebook chunk for the first `n` entries
/// of `cb`. Helper for the crate's roundtrip tests.
pub fn encode_full_chunk(kind: CodebookChunkKind, cb: &Codebook, n: usize, out: &mut Vec<u8>) {
    assert!(n <= 256);
    let entry_size = kind.mode.entry_size();
    let payload_size = n * entry_size;
    let chunk_size = (CHUNK_HEADER_SIZE + payload_size) as u16;
    out.extend_from_slice(&kind.to_id().to_be_bytes());
    out.extend_from_slice(&chunk_size.to_be_bytes());
    let start = out.len();
    out.resize(start + payload_size, 0);
    for i in 0..n {
        let off = start + i * entry_size;
        write_entry(kind.mode, &cb.entries[i], &mut out[off..off + entry_size]);
    }
}

/// Encode a header-only chunk (no payload, signalling "no update").
pub fn encode_header_only_chunk(kind: CodebookChunkKind, out: &mut Vec<u8>) {
    out.extend_from_slice(&kind.to_id().to_be_bytes());
    out.extend_from_slice(&(CHUNK_HEADER_SIZE as u16).to_be_bytes());
}

/// Encode a selective-update chunk that replaces every entry index in
/// `slots` with the corresponding entry from `cb`. `slots` must be in
/// strictly ascending order and every index must be `< 256`.
///
/// The encoded chunk emits one 32-bit flag word per group of 32 slots
/// (groups `0..31`, `32..63`, …, `224..255`). A group with no selected
/// slots is still emitted as a zero flag word **only if** a later group
/// has selected slots; trailing all-zero groups are elided. Per spec
/// §4.2 the encoder MUST NOT emit more than 8 groups.
///
/// FFmpeg 7.1.2 never emits selective-update chunks (spec §4.4); this
/// helper exists for the crate's encoder and roundtrip-test harnesses.
pub fn encode_selective_chunk(
    kind: CodebookChunkKind,
    cb: &Codebook,
    slots: &[u8],
    out: &mut Vec<u8>,
) {
    debug_assert!(slots.windows(2).all(|w| w[0] < w[1]));
    let entry_size = kind.mode.entry_size();
    let mut groups: [(u32, Vec<u8>); 8] = Default::default();
    let mut last_nonempty_group = 0usize;
    for &slot in slots {
        let g = (slot / 32) as usize;
        let bit = slot % 32;
        groups[g].0 |= 1u32 << (31 - bit);
        let entry = &cb.entries[slot as usize];
        let mut entry_bytes = [0u8; 6];
        write_entry(kind.mode, entry, &mut entry_bytes);
        groups[g].1.extend_from_slice(&entry_bytes[..entry_size]);
        last_nonempty_group = last_nonempty_group.max(g);
    }
    // Compute payload size.
    let mut payload_size = 0usize;
    for g in groups.iter().take(last_nonempty_group + 1) {
        payload_size += 4 + g.1.len();
    }
    let chunk_size = (CHUNK_HEADER_SIZE + payload_size) as u16;
    out.extend_from_slice(&kind.to_id().to_be_bytes());
    out.extend_from_slice(&chunk_size.to_be_bytes());
    for g in groups.iter().take(last_nonempty_group + 1) {
        out.extend_from_slice(&g.0.to_be_bytes());
        out.extend_from_slice(&g.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_chunk_ids() {
        let f = CodebookChunkKind::from_id(0x2000).unwrap();
        assert_eq!(
            f,
            CodebookChunkKind {
                which: WhichCodebook::V4,
                style: UpdateStyle::Full,
                mode: PixelMode::Yuv12
            }
        );
        assert_eq!(f.to_id(), 0x2000);

        let f = CodebookChunkKind::from_id(0x2200).unwrap();
        assert_eq!(f.which, WhichCodebook::V1);
        assert_eq!(f.mode, PixelMode::Yuv12);

        let f = CodebookChunkKind::from_id(0x2400).unwrap();
        assert_eq!(f.mode, PixelMode::Gray8);
        assert_eq!(f.which, WhichCodebook::V4);

        let f = CodebookChunkKind::from_id(0x2700).unwrap();
        assert_eq!(f.style, UpdateStyle::Selective);
        assert_eq!(f.which, WhichCodebook::V1);
        assert_eq!(f.mode, PixelMode::Gray8);

        // Vector-chunk codes are not codebook chunks.
        assert!(CodebookChunkKind::from_id(0x3000).is_none());
        // Strip-id codes neither.
        assert!(CodebookChunkKind::from_id(0x1000).is_none());
        // Non-zero low byte is malformed.
        assert!(CodebookChunkKind::from_id(0x2001).is_none());
    }

    /// Spec §3.1 fixture `T1a` — solid red strip's V1 chunk has one
    /// entry with bytes `48 48 48 48 db 5a` mapping to `(72,72,72,72,
    /// -37, +90)`.
    #[test]
    fn applies_t1a_v1_full_replace() {
        let kind = CodebookChunkKind::from_id(0x2200).unwrap();
        let payload = [0x48u8, 0x48, 0x48, 0x48, 0xdb, 0x5a];
        let mut cb = Codebook::default();
        apply_codebook_chunk(kind, &payload, &mut cb).unwrap();
        assert_eq!(
            cb.entries[0],
            CodebookEntry::from_yuv(72, 72, 72, 72, -37, 90)
        );
    }

    /// Selective update — bit 31 of the first flag word sets slot 0.
    #[test]
    fn applies_selective_single_bit() {
        let kind = CodebookChunkKind::from_id(0x2300).unwrap();
        let mut payload = vec![0x80, 0x00, 0x00, 0x00]; // bit 31 = slot 0
        payload.extend_from_slice(&[0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);
        let mut cb = Codebook::default();
        // Pre-fill slot 0 with junk to verify it gets overwritten.
        cb.entries[0] = CodebookEntry::from_yuv(1, 2, 3, 4, 5, 6);
        cb.entries[1] = CodebookEntry::from_yuv(99, 99, 99, 99, 99, 99);
        apply_codebook_chunk(kind, &payload, &mut cb).unwrap();
        assert_eq!(
            cb.entries[0],
            CodebookEntry::from_yuv(0x10, 0x20, 0x30, 0x40, 0x50, 0x60)
        );
        // Slot 1 wasn't selected — preserved.
        assert_eq!(
            cb.entries[1],
            CodebookEntry::from_yuv(99, 99, 99, 99, 99, 99)
        );
    }

    /// Selective update — bit 0 of the first group selects slot 31.
    #[test]
    fn selective_bit_scan_msb_first() {
        let kind = CodebookChunkKind::from_id(0x2300).unwrap();
        let mut payload = vec![0x00, 0x00, 0x00, 0x01]; // bit 0 = slot 31
        payload.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let mut cb = Codebook::default();
        apply_codebook_chunk(kind, &payload, &mut cb).unwrap();
        assert_eq!(cb.entries[31], CodebookEntry::from_yuv(1, 2, 3, 4, 5, 6));
        assert_eq!(cb.entries[0], CodebookEntry::default());
    }

    /// Header-only ("no update") full-replace round-trip: encode then
    /// decode produces an empty payload.
    #[test]
    fn header_only_chunk_is_noop() {
        let kind = CodebookChunkKind::from_id(0x2000).unwrap();
        let mut buf = Vec::new();
        encode_header_only_chunk(kind, &mut buf);
        assert_eq!(buf, vec![0x20, 0x00, 0x00, 0x04]);
    }

    /// Selective update on V4 12-bit YUV (`0x2100`). FFmpeg never emits
    /// `0x2100` in any test corpus configuration (spec §4.4), so this
    /// is a synthesis-only test of the wire-format decode path.
    #[test]
    fn applies_selective_v4_yuv_2100() {
        let kind = CodebookChunkKind::from_id(0x2100).unwrap();
        // Bits 31, 30, 29 = 1 → slots 0, 1, 2 of group 0.
        let mut payload = vec![0xe0, 0x00, 0x00, 0x00];
        payload.extend_from_slice(&[10, 20, 30, 40, 5, 6]);
        payload.extend_from_slice(&[50, 60, 70, 80, 7, 8]);
        payload.extend_from_slice(&[90, 100, 110, 120, 9, 10]);
        let mut cb = Codebook::default();
        apply_codebook_chunk(kind, &payload, &mut cb).unwrap();
        assert_eq!(cb.entries[0], CodebookEntry::from_yuv(10, 20, 30, 40, 5, 6));
        assert_eq!(
            cb.entries[2],
            CodebookEntry::from_yuv(90, 100, 110, 120, 9, 10)
        );
        assert_eq!(cb.entries[3], CodebookEntry::default()); // unchanged
    }

    /// Selective update on V4 grayscale (`0x2500`). 4-byte entries.
    /// Synthesis-only: spec §4.4.
    #[test]
    fn applies_selective_v4_gray_2500() {
        let kind = CodebookChunkKind::from_id(0x2500).unwrap();
        // Bit 30 of group 1 (= slot 33).
        let mut payload = vec![0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00];
        payload.extend_from_slice(&[11, 22, 33, 44]);
        let mut cb = Codebook::default();
        apply_codebook_chunk(kind, &payload, &mut cb).unwrap();
        assert_eq!(cb.entries[33], CodebookEntry::from_y(11, 22, 33, 44));
        assert_eq!(cb.entries[32], CodebookEntry::default());
    }

    /// Selective update on V1 grayscale (`0x2700`). Synthesis-only.
    #[test]
    fn applies_selective_v1_gray_2700() {
        let kind = CodebookChunkKind::from_id(0x2700).unwrap();
        // Bit 0 of group 7 (slot 255).
        let mut payload = vec![0x00; 4 * 7];
        payload.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        payload.extend_from_slice(&[1, 2, 3, 4]);
        let mut cb = Codebook::default();
        apply_codebook_chunk(kind, &payload, &mut cb).unwrap();
        assert_eq!(cb.entries[255], CodebookEntry::from_y(1, 2, 3, 4));
    }

    /// Encoder roundtrip: encode a selective-update chunk + decode.
    #[test]
    fn selective_chunk_encode_roundtrip() {
        let kind = CodebookChunkKind::from_id(0x2300).unwrap();
        let mut cb = Codebook::default();
        cb.entries[0] = CodebookEntry::from_yuv(1, 2, 3, 4, 5, 6);
        cb.entries[33] = CodebookEntry::from_yuv(7, 8, 9, 10, 11, 12);
        cb.entries[200] = CodebookEntry::from_yuv(13, 14, 15, 16, -1, -2);
        let slots = [0u8, 33, 200];
        let mut buf = Vec::new();
        encode_selective_chunk(kind, &cb, &slots, &mut buf);
        // Strip header.
        let payload = &buf[CHUNK_HEADER_SIZE..];
        let mut out = Codebook::default();
        apply_codebook_chunk(kind, payload, &mut out).unwrap();
        for &s in &slots {
            assert_eq!(out.entries[s as usize], cb.entries[s as usize]);
        }
        // Untouched entries.
        assert_eq!(out.entries[1], CodebookEntry::default());
    }

    /// Sega Saturn deviant 0x2000 chunk: 0x5FC-byte payload that
    /// contains 255 6-byte vectors + 2 trailing pad bytes. Reference:
    /// `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` line 143
    /// ("the solution in this case is to unpack 255 6-byte vectors and
    /// then skip 2 bytes"). Standard `apply_codebook_chunk` must reject
    /// the malformed length; `apply_codebook_chunk_with(..., true)`
    /// must accept it and decode exactly 255 entries.
    #[test]
    fn deviant_full_chunk_tolerates_trailing_pad() {
        let kind = CodebookChunkKind::from_id(0x2000).unwrap();
        // Build 255 distinct entries + 2 trailing bytes.
        let mut payload = Vec::with_capacity(255 * 6 + 2);
        for i in 0..255u16 {
            // Make each entry recognisable by index.
            let y = (i & 0xff) as u8;
            payload.extend_from_slice(&[
                y,
                y.wrapping_add(1),
                y.wrapping_add(2),
                y.wrapping_add(3),
                0,
                0,
            ]);
        }
        payload.extend_from_slice(&[0xAB, 0xCD]); // garbage pad
        assert_eq!(payload.len(), 0x5FC);

        // Standard path rejects.
        let mut cb_strict = Codebook::default();
        assert!(apply_codebook_chunk(kind, &payload, &mut cb_strict).is_err());

        // Deviant path accepts, decodes 255 entries.
        let mut cb = Codebook::default();
        apply_codebook_chunk_with(kind, &payload, &mut cb, true).unwrap();
        assert_eq!(cb.entries[0], CodebookEntry::from_yuv(0, 1, 2, 3, 0, 0));
        assert_eq!(
            cb.entries[254],
            CodebookEntry::from_yuv(254, 255, 0, 1, 0, 0)
        );
        // Slot 255 must be untouched (still default).
        assert_eq!(cb.entries[255], CodebookEntry::default());
    }

    /// Tolerating-trailing on a clean payload must still decode the
    /// usual count and leave no garbage.
    #[test]
    fn deviant_tolerate_trailing_no_op_when_clean() {
        let kind = CodebookChunkKind::from_id(0x2000).unwrap();
        let mut payload = Vec::new();
        for i in 0..16u8 {
            payload.extend_from_slice(&[i, i, i, i, 0, 0]);
        }
        let mut cb = Codebook::default();
        apply_codebook_chunk_with(kind, &payload, &mut cb, true).unwrap();
        assert_eq!(cb.entries[0], CodebookEntry::from_yuv(0, 0, 0, 0, 0, 0));
        assert_eq!(
            cb.entries[15],
            CodebookEntry::from_yuv(15, 15, 15, 15, 0, 0)
        );
    }

    #[test]
    fn full_chunk_roundtrip() {
        let kind = CodebookChunkKind::from_id(0x2000).unwrap();
        let mut cb = Codebook::default();
        cb.entries[0] = CodebookEntry::from_yuv(10, 20, 30, 40, -5, 7);
        cb.entries[1] = CodebookEntry::from_yuv(50, 60, 70, 80, 11, -13);
        let mut bytes = Vec::new();
        encode_full_chunk(kind, &cb, 2, &mut bytes);
        // Strip the 4-byte chunk header to obtain the payload.
        let payload = &bytes[CHUNK_HEADER_SIZE..];
        let mut out = Codebook::default();
        apply_codebook_chunk(kind, payload, &mut out).unwrap();
        assert_eq!(out.entries[0], cb.entries[0]);
        assert_eq!(out.entries[1], cb.entries[1]);
    }
}
