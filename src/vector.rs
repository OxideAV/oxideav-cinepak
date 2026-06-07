//! Cinepak vector chunks and macroblock dispatch.
//!
//! Wire-format reference:
//! `docs/video/cinepak/spec/03-vectors-and-macroblocks.md`. Three
//! vector-chunk codes are defined:
//!
//! - `0x3200` — V1-only intra. No flag word; one byte per macroblock.
//! - `0x3000` — Mixed V1/V4 intra. Per-group 32-bit flag word; bit
//!   `0` ⇒ V1 (1 index byte), bit `1` ⇒ V4 (4 index bytes).
//! - `0x3100` — Inter with skip codes. Variable-length codes `0` =
//!   skip / `10` = V1 / `11` = V4 packed bit-wise into the flag-word
//!   stream; codes can span flag-word boundaries.

use crate::error::{CinepakError, Result};

pub const VECTOR_CHUNK_V1_ONLY: u16 = 0x3200;
pub const VECTOR_CHUNK_INTRA: u16 = 0x3000;
pub const VECTOR_CHUNK_INTER: u16 = 0x3100;

/// One macroblock decoded out of a vector chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mb {
    /// Skip this macroblock — copy the corresponding 4×4 block from the
    /// previous frame's reconstructed output. Only emitted by `0x3100`.
    Skip,
    /// V1-coded — one V1 codebook index.
    V1(u8),
    /// V4-coded — four V4 codebook indices in row-major scan-order
    /// `(top-left, top-right, bottom-left, bottom-right)` of the 2×2
    /// sub-block arrangement.
    V4([u8; 4]),
}

/// Decode a vector chunk's payload into a flat list of `mb_count`
/// macroblock descriptors.
///
/// `chunk_id` is the 16-bit chunk type; `payload` is the chunk's bytes
/// **excluding** the 4-byte chunk header.
pub fn decode_vector_chunk(chunk_id: u16, payload: &[u8], mb_count: usize) -> Result<Vec<Mb>> {
    match chunk_id {
        VECTOR_CHUNK_V1_ONLY => decode_v1_only(payload, mb_count),
        VECTOR_CHUNK_INTRA => decode_mixed_intra(payload, mb_count),
        VECTOR_CHUNK_INTER => decode_inter(payload, mb_count),
        _ => Err(CinepakError::invalid(format!(
            "unknown vector chunk id 0x{chunk_id:04x}"
        ))),
    }
}

fn decode_v1_only(payload: &[u8], mb_count: usize) -> Result<Vec<Mb>> {
    if payload.len() != mb_count {
        return Err(CinepakError::invalid(format!(
            "0x3200 payload len {} != mb_count {mb_count}",
            payload.len()
        )));
    }
    Ok(payload.iter().map(|&b| Mb::V1(b)).collect())
}

fn decode_mixed_intra(payload: &[u8], mb_count: usize) -> Result<Vec<Mb>> {
    let mut out = Vec::with_capacity(mb_count);
    let mut p = 0usize;
    while out.len() < mb_count {
        if payload.len() - p < 4 {
            return Err(CinepakError::invalid("0x3000 truncated mid-flag-word"));
        }
        let flag = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]]);
        p += 4;
        // First scan: classify up to 32 macroblocks of this group from
        // the flag word's MSB-first bits.
        let group_end = (out.len() + 32).min(mb_count);
        let mut kinds = Vec::with_capacity(group_end - out.len());
        let mut bit_pos = 0usize;
        while out.len() + kinds.len() < group_end {
            let mask = 1u32 << (31 - bit_pos);
            kinds.push(flag & mask != 0); // true = V4, false = V1
            bit_pos += 1;
        }
        // Second scan: read the index bytes for this group's macroblocks.
        for is_v4 in kinds {
            if is_v4 {
                if payload.len() - p < 4 {
                    return Err(CinepakError::invalid("0x3000 truncated mid-V4-index"));
                }
                let idx = [payload[p], payload[p + 1], payload[p + 2], payload[p + 3]];
                p += 4;
                out.push(Mb::V4(idx));
            } else {
                if payload.len() - p < 1 {
                    return Err(CinepakError::invalid("0x3000 truncated mid-V1-index"));
                }
                out.push(Mb::V1(payload[p]));
                p += 1;
            }
        }
    }
    Ok(out)
}

/// Decode the inter form (`0x3100`) with VLC codes `0` / `10` / `11`.
///
/// The grammar (spec §3.3): each macroblock takes 1 or 2 bits of the
/// flag-word stream. A `0` bit codes a SKIP. A `1` bit followed by a
/// `0` codes V1; a `1` followed by `1` codes V4. The selector bit may
/// land in the **next** flag word — when only the leading `1` fits in
/// the current flag word, the next flag word's MSB is read as the
/// selector before any of its remaining bits are interpreted as
/// macroblock-codes.
///
/// Per spec §3.3 step 5, the index bytes for a macroblock whose code
/// straddles a flag-word boundary appear in the **next** group's
/// index-data block (i.e. with the flag word that carried the
/// selector bit). The decoder defers pushing the macroblock into
/// `out` until the next iteration so its index bytes are read with
/// the correct group.
fn decode_inter(payload: &[u8], mb_count: usize) -> Result<Vec<Mb>> {
    let mut out = Vec::with_capacity(mb_count);
    let mut p = 0usize;
    // When `pending_selector == true` the previous flag word ended on
    // a leading `1`; the current flag word's MSB carries the V1/V4
    // selector and the macroblock's index bytes belong to **this**
    // group's index data. The mb is pushed into `out` here, not in
    // the previous iteration.
    let mut pending_selector = false;
    while out.len() < mb_count {
        if payload.len() - p < 4 {
            return Err(CinepakError::invalid("0x3100 truncated mid-flag-word"));
        }
        let flag = u32::from_be_bytes([payload[p], payload[p + 1], payload[p + 2], payload[p + 3]]);
        p += 4;
        let mut bit = 0u32;
        // Index of the first mb that belongs to this group; its idx
        // bytes are read in step 3 below.
        let group_start_in_out = out.len();

        // 1) Resolve any pending selector from the *previous* flag word
        //    by pushing the deferred macroblock into `out` *now*. Its
        //    index bytes will be read in step 3 of this iteration with
        //    the rest of this group's idx data.
        if pending_selector {
            if bit >= 32 {
                return Err(CinepakError::invalid(
                    "0x3100 flag word has no room for pending selector",
                ));
            }
            let mask = 1u32 << (31 - bit);
            bit += 1;
            let is_v4 = flag & mask != 0;
            if is_v4 {
                out.push(Mb::V4([0, 0, 0, 0]));
            } else {
                out.push(Mb::V1(0));
            }
            pending_selector = false;
        }

        // 2) Decode further codes from this flag word, until either
        //    mb_count is reached or the bits run out.
        while out.len() < mb_count && bit < 32 {
            let mask = 1u32 << (31 - bit);
            bit += 1;
            if flag & mask == 0 {
                // SKIP — no index bytes.
                out.push(Mb::Skip);
                continue;
            }
            // Leading `1` — need one more bit as the V1/V4 selector.
            if bit >= 32 {
                // Selector bit must come from the next flag word; defer
                // pushing this macroblock until step 1 of the next
                // iteration so its index bytes are read in the next
                // group (where the encoder placed them).
                pending_selector = true;
                break;
            }
            let sel_mask = 1u32 << (31 - bit);
            bit += 1;
            let is_v4 = flag & sel_mask != 0;
            if is_v4 {
                out.push(Mb::V4([0, 0, 0, 0]));
            } else {
                out.push(Mb::V1(0));
            }
        }

        // 3) Read this group's index data — 1 byte per V1 mb, 4 bytes
        //    per V4 mb. Iterate over the just-classified macroblocks
        //    (group_start_in_out..out.len()), in scan order. The mb
        //    that resolved a pending selector is the first in this
        //    range, so its idx bytes are read with the rest.
        for mb in out.iter_mut().skip(group_start_in_out) {
            match mb {
                Mb::Skip => {}
                Mb::V1(idx) => {
                    if payload.len() - p < 1 {
                        return Err(CinepakError::invalid("0x3100 truncated mid-V1-index"));
                    }
                    *idx = payload[p];
                    p += 1;
                }
                Mb::V4(idx) => {
                    if payload.len() - p < 4 {
                        return Err(CinepakError::invalid("0x3100 truncated mid-V4-index"));
                    }
                    idx.copy_from_slice(&payload[p..p + 4]);
                    p += 4;
                }
            }
        }
    }
    if pending_selector {
        return Err(CinepakError::invalid(
            "0x3100 ended with a dangling pending selector",
        ));
    }
    Ok(out)
}

// ---------- V1OnlyMacroblocks iterator (typed §3.1 walker) ---------------

/// One macroblock yielded by [`V1OnlyMacroblocks`].
///
/// `0x3200` payloads are the simplest of the three vector-chunk codes
/// (spec §3.1 of `docs/video/cinepak/spec/03-vectors-and-macroblocks.md`):
/// no flag word, one byte per macroblock, each byte a V1 codebook
/// index. `V1MacroblockEntry` exposes the 0-based macroblock scan-order
/// index (spec §1.1) alongside the codebook index byte so callers can
/// correlate the wire byte with its macroblock position without
/// recomputing the count externally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V1MacroblockEntry {
    /// 0-based macroblock index in scan order (spec §1.1).
    pub index: u16,
    /// V1 codebook index byte for this macroblock — the single payload
    /// byte at offset `index` within the chunk payload.
    pub codebook_index: u8,
}

/// Iterator over the macroblock entries inside a `0x3200` V1-only
/// vector-chunk payload.
///
/// Wire-format reference: spec §3.1 of
/// `docs/video/cinepak/spec/03-vectors-and-macroblocks.md`. A
/// `0x3200` chunk's payload is exactly `mb_count` bytes, each byte a
/// V1 codebook index for one macroblock in row-major scan order (spec
/// §1.1). There is no flag word.
///
/// This iterator is the next layer below the round-243
/// [`crate::codebook::StripChunks`] chunk-stream iterator for the
/// simplest of the three vector-chunk codes — given a
/// [`crate::codebook::StripChunkEntry`] whose `kind` resolves to
/// [`crate::codebook::VectorChunkKind::IntraV1Only`], a caller can
/// take that entry's `payload` slice and walk it macroblock-by-
/// macroblock without invoking the codebook + vector decode stack of
/// [`decode_vector_chunk`].
///
/// Read-only and content-agnostic — the iterator does not consult
/// codebook entries, perform V1 macroblock expansion (spec §4), or
/// touch pixel data. Useful for:
///
/// - validators / wire-format introspection tools that want to
///   correlate each `0x3200` payload byte with its macroblock scan
///   position;
/// - fuzz harnesses that need a typed per-macroblock surface to
///   classify malformed-vs-well-formed payloads;
/// - container code (Sega FILM, AVI sub-stream) that wants to walk a
///   strip's macroblock-level coverage without pulling in the full
///   codec dependency.
///
/// ### Construction
///
/// [`V1OnlyMacroblocks::new`] verifies the spec §3.1
/// `payload.len() == mb_count` invariant and returns an error
/// otherwise; the resulting iterator is infallible thereafter —
/// `next()` yields `Some(Ok(_))` exactly `mb_count` times and then
/// `None`.
///
/// ### Iterator semantics
///
/// `next()` yields one [`V1MacroblockEntry`] per byte of the payload
/// until exhaustion. [`Iterator::size_hint`] reports the exact
/// remaining count; [`ExactSizeIterator`] is implemented so callers
/// can call [`Iterator::len`] without consuming the iterator.
#[derive(Debug)]
pub struct V1OnlyMacroblocks<'a> {
    /// Chunk payload slice; length equals `mb_count` after the §3.1
    /// length-equality check in [`V1OnlyMacroblocks::new`].
    payload: &'a [u8],
    /// Total macroblock count for the strip; recorded for
    /// [`Self::mb_count`] reporting and `size_hint` arithmetic.
    mb_count: usize,
    /// Byte offset of the next macroblock entry within `payload`.
    /// Equivalently, the 0-based index of the next macroblock to
    /// yield.
    cursor: usize,
}

impl<'a> V1OnlyMacroblocks<'a> {
    /// Build a V1-only macroblock iterator over the chunk payload
    /// `payload` for a strip of `mb_count` macroblocks.
    ///
    /// `payload` is the `chunk_size - 4` bytes that follow the 4-byte
    /// `(chunk_id, chunk_size)` chunk header — equivalently the
    /// `payload` field of a [`crate::codebook::StripChunkEntry`] whose
    /// `kind` resolves to [`crate::codebook::VectorChunkKind::IntraV1Only`].
    ///
    /// Returns an error when the spec §3.1 invariant
    /// `payload.len() == mb_count` is violated (payload shorter or
    /// longer than `mb_count` bytes); on the happy path the resulting
    /// iterator is infallible.
    pub fn new(payload: &'a [u8], mb_count: usize) -> Result<Self> {
        if payload.len() != mb_count {
            return Err(CinepakError::invalid(format!(
                "0x3200 payload len {} != mb_count {mb_count}",
                payload.len()
            )));
        }
        Ok(Self {
            payload,
            mb_count,
            cursor: 0,
        })
    }

    /// Byte offset of the next macroblock entry within the payload.
    /// Equivalently, the 0-based index of the next macroblock to
    /// yield. Useful for error reporting when this iterator is
    /// composed inside a higher-level frame walker.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Number of macroblocks yet to be yielded.
    pub fn remaining(&self) -> usize {
        self.mb_count.saturating_sub(self.cursor)
    }

    /// Total macroblock count for the strip (the value passed to
    /// [`Self::new`]; equivalently the payload length).
    pub fn mb_count(&self) -> usize {
        self.mb_count
    }

    /// Underlying chunk payload slice — useful for byte-level
    /// validators and round-trip checks.
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

impl<'a> Iterator for V1OnlyMacroblocks<'a> {
    type Item = Result<V1MacroblockEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor >= self.mb_count {
            return None;
        }
        // §3.1 length-equality invariant is upheld by `new`: payload
        // is exactly `mb_count` bytes long, so the indexed access
        // here is always in-bounds while `cursor < mb_count`.
        let entry = V1MacroblockEntry {
            index: self.cursor as u16,
            codebook_index: self.payload[self.cursor],
        };
        self.cursor += 1;
        Some(Ok(entry))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.remaining();
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for V1OnlyMacroblocks<'a> {
    fn len(&self) -> usize {
        self.remaining()
    }
}

// ---------- MixedIntraMacroblocks iterator (typed §3.2 walker) -----------

/// One macroblock yielded by [`MixedIntraMacroblocks`].
///
/// `0x3000` payloads encode each macroblock as either a single V1
/// codebook index byte or a four-byte V4 codebook index quadruple,
/// classified by a per-group flag word (spec §3.2 of
/// `docs/video/cinepak/spec/03-vectors-and-macroblocks.md`). The
/// per-entry [`MixedIntraEntry::kind`] selector preserves that
/// distinction alongside the 0-based scan-order index (spec §1.1) so
/// callers can correlate each wire byte / wire quadruple with its
/// macroblock position without recomputing the group-boundary
/// arithmetic externally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixedIntraEntry {
    /// 0-based macroblock index in scan order (spec §1.1).
    pub index: u16,
    /// Per-macroblock V1 / V4 classification with the corresponding
    /// codebook index bytes copied out of the chunk payload.
    pub kind: MixedIntraMb,
}

/// Per-macroblock V1 / V4 classification yielded by
/// [`MixedIntraMacroblocks`].
///
/// `0x3000` flag-word bits select per-MB coding mode: bit `0` ⇒ V1
/// (1 index byte), bit `1` ⇒ V4 (4 index bytes); spec §3.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixedIntraMb {
    /// V1-coded macroblock — single codebook index byte.
    V1(u8),
    /// V4-coded macroblock — four codebook indices in row-major
    /// 2×2-subblock scan order `(top-left, top-right, bottom-left,
    /// bottom-right)`; spec §5.
    V4([u8; 4]),
}

/// Iterator over the macroblock entries inside a `0x3000` mixed-intra
/// vector-chunk payload.
///
/// Wire-format reference: spec §3.2 of
/// `docs/video/cinepak/spec/03-vectors-and-macroblocks.md`. A
/// `0x3000` payload is a sequence of one or more **groups**, each
/// starting with a 4-byte big-endian flag word. Each of the flag
/// word's 32 bits, scanned MSB-first, classifies one macroblock as
/// either V1 (bit clear) or V4 (bit set); the index bytes for the
/// group's macroblocks follow the flag word in scan order (one byte
/// per V1, four bytes per V4). A group covers exactly 32 macroblocks
/// unless the strip's macroblock count is exhausted before 32.
///
/// This iterator is the §3.2 mirror of round-246's
/// [`V1OnlyMacroblocks`] — given a [`crate::codebook::StripChunkEntry`]
/// whose `kind` resolves to [`crate::codebook::VectorChunkKind::IntraMixed`],
/// a caller can take that entry's `payload` slice and walk it
/// macroblock-by-macroblock without invoking the codebook + vector
/// decode stack of [`decode_vector_chunk`].
///
/// Read-only and content-agnostic — the iterator does not consult
/// codebook entries, perform V1 / V4 macroblock expansion (spec
/// §§4–5), or touch pixel data. Useful for:
///
/// - validators / wire-format introspection tools that want to
///   correlate each `0x3000` payload byte with its macroblock scan
///   position and V1 / V4 classification;
/// - fuzz harnesses that need a typed per-macroblock surface to
///   classify malformed-vs-well-formed payloads;
/// - container code (Sega FILM, AVI sub-stream) that wants to walk a
///   strip's macroblock-level coverage without pulling in the full
///   codec dependency.
///
/// ### Construction
///
/// [`MixedIntraMacroblocks::new`] performs no eager length check —
/// the spec §3.2 per-group byte size depends on the in-group V1 /
/// V4 mix, so payload-vs-`mb_count` consistency can only be
/// confirmed during the walk. The iterator therefore reports
/// truncation per-yield as `Some(Err(_))` and self-fuses afterwards
/// (subsequent `next()` calls return `None`).
///
/// ### Iterator semantics
///
/// `next()` yields one [`MixedIntraEntry`] per macroblock until
/// `mb_count` are yielded or the payload is exhausted / malformed.
/// On error the iterator emits `Some(Err(_))` exactly once and then
/// fuses to `None`. [`Iterator::size_hint`] reports `(remaining,
/// Some(remaining))` based on the original `mb_count`.
#[derive(Debug)]
pub struct MixedIntraMacroblocks<'a> {
    /// Chunk payload slice (chunk_size - 4 bytes following the 4-byte
    /// chunk header).
    payload: &'a [u8],
    /// Total macroblock count for the strip; recorded for
    /// [`Self::mb_count`] reporting and `size_hint` arithmetic.
    mb_count: usize,
    /// Byte offset of the next payload read within `payload`. The next
    /// read may be a 4-byte flag word (at a group boundary) or an
    /// index byte / quadruple within the current group.
    cursor: usize,
    /// 0-based index of the next macroblock to yield (equivalently,
    /// the count of macroblocks already yielded).
    next_mb: usize,
    /// Per-group V1/V4 classification buffer, populated from the
    /// latest flag word in scan order at MSB-first bit positions
    /// `[0, group_kinds_len)`. `group_kinds[i]` = `true` ⇒ V4,
    /// `false` ⇒ V1; spec §3.2.
    group_kinds: [bool; 32],
    /// Number of valid entries in [`Self::group_kinds`] for the
    /// current group — equals `(mb_count - next_mb).min(32)` at the
    /// moment the flag word was loaded.
    group_kinds_len: u8,
    /// Index of the next entry to consume from
    /// [`Self::group_kinds`] within the current group.
    group_cursor: u8,
    /// One-shot fuse — set when an error is yielded so subsequent
    /// `next()` calls return `None`.
    fused: bool,
}

impl<'a> MixedIntraMacroblocks<'a> {
    /// Build a mixed-intra macroblock iterator over the chunk payload
    /// `payload` for a strip of `mb_count` macroblocks.
    ///
    /// `payload` is the `chunk_size - 4` bytes that follow the 4-byte
    /// `(chunk_id, chunk_size)` chunk header — equivalently the
    /// `payload` field of a [`crate::codebook::StripChunkEntry`] whose
    /// `kind` resolves to [`crate::codebook::VectorChunkKind::IntraMixed`].
    ///
    /// No eager length check is performed; per-group byte sizes
    /// depend on the V1 / V4 mix within the group. Truncation is
    /// reported per-yield as `Some(Err(_))` and the iterator fuses
    /// to `None` afterwards.
    pub fn new(payload: &'a [u8], mb_count: usize) -> Self {
        Self {
            payload,
            mb_count,
            cursor: 0,
            next_mb: 0,
            group_kinds: [false; 32],
            group_kinds_len: 0,
            group_cursor: 0,
            fused: false,
        }
    }

    /// Byte offset of the next payload read within the payload slice.
    /// Useful for error reporting when this iterator is composed
    /// inside a higher-level frame walker.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Number of macroblocks yet to be yielded.
    pub fn remaining(&self) -> usize {
        self.mb_count.saturating_sub(self.next_mb)
    }

    /// Total macroblock count for the strip (the value passed to
    /// [`Self::new`]).
    pub fn mb_count(&self) -> usize {
        self.mb_count
    }

    /// Underlying chunk payload slice — useful for byte-level
    /// validators and round-trip checks.
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Read a 4-byte big-endian flag word at the current cursor and
    /// classify up to 32 macroblocks (or fewer, when the strip's
    /// macroblock count is exhausted before 32) into
    /// [`Self::group_kinds`]. Advances `self.cursor` by 4.
    fn load_next_group(&mut self) -> Result<()> {
        if self.payload.len() - self.cursor < 4 {
            return Err(CinepakError::invalid(format!(
                "0x3000 truncated mid-flag-word at byte {}/{}",
                self.cursor,
                self.payload.len()
            )));
        }
        let flag = u32::from_be_bytes([
            self.payload[self.cursor],
            self.payload[self.cursor + 1],
            self.payload[self.cursor + 2],
            self.payload[self.cursor + 3],
        ]);
        self.cursor += 4;
        // Classify up to 32 macroblocks (or fewer if the strip's
        // macroblock count is exhausted before 32 — spec §3.2: "A
        // group covers exactly 32 macroblocks unless the strip's
        // macroblock count is exhausted before 32.")
        let group_size = (self.mb_count - self.next_mb).min(32);
        for bit_pos in 0..group_size {
            let mask = 1u32 << (31 - bit_pos);
            self.group_kinds[bit_pos] = flag & mask != 0;
        }
        self.group_kinds_len = group_size as u8;
        self.group_cursor = 0;
        Ok(())
    }
}

impl<'a> Iterator for MixedIntraMacroblocks<'a> {
    type Item = Result<MixedIntraEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fused {
            return None;
        }
        if self.next_mb >= self.mb_count {
            return None;
        }
        // Refill the per-group classification buffer at every group
        // boundary.
        if self.group_cursor >= self.group_kinds_len {
            if let Err(e) = self.load_next_group() {
                self.fused = true;
                return Some(Err(e));
            }
        }
        let is_v4 = self.group_kinds[self.group_cursor as usize];
        self.group_cursor += 1;
        let kind = if is_v4 {
            if self.payload.len() - self.cursor < 4 {
                self.fused = true;
                return Some(Err(CinepakError::invalid(format!(
                    "0x3000 truncated mid-V4-index at byte {}/{} (mb {})",
                    self.cursor,
                    self.payload.len(),
                    self.next_mb
                ))));
            }
            let idx = [
                self.payload[self.cursor],
                self.payload[self.cursor + 1],
                self.payload[self.cursor + 2],
                self.payload[self.cursor + 3],
            ];
            self.cursor += 4;
            MixedIntraMb::V4(idx)
        } else {
            if self.payload.len() - self.cursor < 1 {
                self.fused = true;
                return Some(Err(CinepakError::invalid(format!(
                    "0x3000 truncated mid-V1-index at byte {}/{} (mb {})",
                    self.cursor,
                    self.payload.len(),
                    self.next_mb
                ))));
            }
            let idx = self.payload[self.cursor];
            self.cursor += 1;
            MixedIntraMb::V1(idx)
        };
        let entry = MixedIntraEntry {
            index: self.next_mb as u16,
            kind,
        };
        self.next_mb += 1;
        Some(Ok(entry))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.fused {
            return (0, Some(0));
        }
        let remaining = self.remaining();
        (remaining, Some(remaining))
    }
}

// ---------- Encoders (used by self-roundtrip tests) ----------------------

/// Encode a `0x3200` V1-only intra vector chunk's payload (no header).
pub fn encode_v1_only_payload(mbs: &[Mb], out: &mut Vec<u8>) -> Result<()> {
    for mb in mbs {
        match mb {
            Mb::V1(idx) => out.push(*idx),
            _ => {
                return Err(CinepakError::other(
                    "0x3200 encoder only accepts V1 macroblocks",
                ))
            }
        }
    }
    Ok(())
}

/// Encode a `0x3000` mixed-intra vector chunk's payload (no header).
pub fn encode_mixed_intra_payload(mbs: &[Mb], out: &mut Vec<u8>) -> Result<()> {
    let mut i = 0;
    while i < mbs.len() {
        let group_end = (i + 32).min(mbs.len());
        let mut flag = 0u32;
        // First pass: build the flag word.
        for (bit_pos, mb) in mbs[i..group_end].iter().enumerate() {
            match mb {
                Mb::V4(_) => flag |= 1u32 << (31 - bit_pos),
                Mb::V1(_) => {}
                Mb::Skip => {
                    return Err(CinepakError::other(
                        "0x3000 encoder rejects Skip macroblocks",
                    ));
                }
            }
        }
        out.extend_from_slice(&flag.to_be_bytes());
        // Second pass: emit index bytes.
        for mb in &mbs[i..group_end] {
            match mb {
                Mb::V1(idx) => out.push(*idx),
                Mb::V4(idx) => out.extend_from_slice(idx),
                Mb::Skip => unreachable!(),
            }
        }
        i = group_end;
    }
    Ok(())
}

/// Encode a `0x3100` inter vector chunk's payload (no header).
///
/// Bit grammar: `0` = SKIP, `10` = V1, `11` = V4. Codes may straddle
/// flag-word boundaries; the encoder tracks a per-group bit cursor and
/// flushes the current flag word when 32 bits are consumed (reserving
/// a partial trailing `1` for the next group's selector).
pub fn encode_inter_payload(mbs: &[Mb], out: &mut Vec<u8>) -> Result<()> {
    // We build a list of `(opcode_bits)` pairs and a parallel list of
    // index-byte payloads, then assemble groups.
    //
    // To keep the encoder simple, we use a "natural" packing that mirrors
    // the decoder: emit codes left-to-right, and at every 32-bit
    // boundary flush a flag word + its group's index bytes.
    let mut bit_buffer = 0u64; // up to 64 bits in flight
    let mut bit_count = 0u32;
    // Pending selector that didn't fit in the current flag word.
    let mut pending_selector: Option<bool> = None;
    let mut group_indices: Vec<Vec<u8>> = Vec::new();
    let mut all_groups: Vec<(u32, Vec<Vec<u8>>)> = Vec::new();
    let mut emitted_macroblocks_in_group = 0usize;

    let flush_group = |bit_buffer: &mut u64,
                       bit_count: &mut u32,
                       pending_selector: &mut Option<bool>,
                       group_indices: &mut Vec<Vec<u8>>,
                       all_groups: &mut Vec<(u32, Vec<Vec<u8>>)>,
                       emitted_macroblocks_in_group: &mut usize| {
        // Take exactly 32 bits, MSB-first, from `bit_buffer`.
        let flag = ((*bit_buffer >> (*bit_count - 32)) & 0xffff_ffff) as u32;
        *bit_count -= 32;
        // Mask out the consumed bits: keep only the lower bit_count bits.
        if *bit_count > 0 {
            *bit_buffer &= (1u64 << *bit_count) - 1;
        } else {
            *bit_buffer = 0;
        }
        all_groups.push((flag, std::mem::take(group_indices)));
        let _ = pending_selector;
        *emitted_macroblocks_in_group = 0;
    };

    let _ = (
        &mut bit_buffer,
        &mut bit_count,
        &mut pending_selector,
        &mut group_indices,
        &mut all_groups,
        &mut emitted_macroblocks_in_group,
    );

    // Use a manual loop because the closure-capture approach above is
    // awkward; rewrite straightforwardly:
    let _ = flush_group;
    let mut bit_buffer: u64 = 0;
    let mut bit_count: u32 = 0;
    let mut group_indices: Vec<Vec<u8>> = Vec::new();
    let mut all_groups: Vec<(u32, Vec<Vec<u8>>)> = Vec::new();

    for mb in mbs {
        // Append code bits and remember whether the leading `1` was the
        // last bit of the current flag word — if so, the selector bit
        // belongs to the *next* group's index data tail.
        let (code, code_len, idx_bytes): (u32, u32, Option<Vec<u8>>) = match mb {
            Mb::Skip => (0, 1, None),
            Mb::V1(i) => (0b10, 2, Some(vec![*i])),
            Mb::V4(idx) => (0b11, 2, Some(idx.to_vec())),
        };
        // Push into `bit_buffer` MSB-first (high end of the buffer).
        bit_buffer = (bit_buffer << code_len) | u64::from(code);
        bit_count += code_len;

        // If the V1/V4 selector bit straddled a 32-bit boundary, we
        // must defer this macroblock's index bytes to the *next*
        // group. Detect that case: the macroblock contributed `code_len
        // == 2` bits, and bit_count after appending is between 33 and
        // 32+1 = 33 with the leading `1` falling at the boundary.
        let straddled = code_len == 2 && bit_count >= 33 && (bit_count - 2) < 32;
        // Whose group does this macroblock's index data belong to?
        if let Some(idx) = idx_bytes {
            if straddled {
                // Defer: push into a "next group" bucket. We do this by
                // flushing the current group first (so this idx goes
                // into the new, empty group_indices).
                while bit_count >= 32 {
                    let flag = ((bit_buffer >> (bit_count - 32)) & 0xffff_ffff) as u32;
                    bit_count -= 32;
                    bit_buffer &= if bit_count > 0 {
                        (1u64 << bit_count) - 1
                    } else {
                        0
                    };
                    all_groups.push((flag, std::mem::take(&mut group_indices)));
                }
                group_indices.push(idx);
            } else {
                group_indices.push(idx);
                while bit_count >= 32 {
                    let flag = ((bit_buffer >> (bit_count - 32)) & 0xffff_ffff) as u32;
                    bit_count -= 32;
                    bit_buffer &= if bit_count > 0 {
                        (1u64 << bit_count) - 1
                    } else {
                        0
                    };
                    all_groups.push((flag, std::mem::take(&mut group_indices)));
                }
            }
        } else {
            // Skip — no index bytes.
            while bit_count >= 32 {
                let flag = ((bit_buffer >> (bit_count - 32)) & 0xffff_ffff) as u32;
                bit_count -= 32;
                bit_buffer &= if bit_count > 0 {
                    (1u64 << bit_count) - 1
                } else {
                    0
                };
                all_groups.push((flag, std::mem::take(&mut group_indices)));
            }
        }
    }
    // Pad remaining bits to 32 with zeros.
    if bit_count > 0 {
        let pad = 32 - bit_count;
        bit_buffer <<= pad;
        let flag = (bit_buffer & 0xffff_ffff) as u32;
        all_groups.push((flag, std::mem::take(&mut group_indices)));
    } else if !group_indices.is_empty() {
        // Shouldn't happen: any pending indices belong to a flag word
        // we already flushed (when bit_count was >= 32). But handle it
        // defensively by padding with a zero flag word.
        all_groups.push((0, std::mem::take(&mut group_indices)));
    }

    for (flag, idx_groups) in all_groups {
        out.extend_from_slice(&flag.to_be_bytes());
        for idx in idx_groups {
            out.extend_from_slice(&idx);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §3.1 fixture `Y6` — 16-MB strip, V1-only chunk.
    #[test]
    fn decodes_v1_only() {
        let payload: Vec<u8> = (0..16).collect();
        let mbs = decode_vector_chunk(VECTOR_CHUNK_V1_ONLY, &payload, 16).unwrap();
        for (i, m) in mbs.iter().enumerate() {
            assert_eq!(*m, Mb::V1(i as u8));
        }
    }

    /// Spec §3.2 fixture `Y9` — all-V4 16-MB strip; flag word top 16
    /// bits are `0xffff` (set), low 16 unused.
    #[test]
    fn decodes_mixed_intra_all_v4() {
        let mut payload = vec![0xff, 0xff, 0x00, 0x00];
        for i in 0..16 {
            payload.extend_from_slice(&[i as u8, i as u8 + 1, i as u8 + 2, i as u8 + 3]);
        }
        let mbs = decode_vector_chunk(VECTOR_CHUNK_INTRA, &payload, 16).unwrap();
        for (i, m) in mbs.iter().enumerate() {
            assert_eq!(*m, Mb::V4([i as u8, i as u8 + 1, i as u8 + 2, i as u8 + 3]));
        }
    }

    /// Spec §3.2 fixture `Y12` — checkerboard V1/V4. Flag word top
    /// 16 bits = `0101 1010 0101 1010` = `0x5a5a`. 8 V1 + 8 V4.
    #[test]
    fn decodes_checkerboard_intra() {
        // V1 at scan-order positions where the bit is 0; V4 at bit=1.
        // Pattern 0x5a5a in MSB-first scan: 0,1,0,1, 1,0,1,0, 0,1,0,1, 1,0,1,0
        // i.e. mb[0]=V1, mb[1]=V4, mb[2]=V1, mb[3]=V4, mb[4]=V4, mb[5]=V1, ...
        let mut payload = vec![0x5a, 0x5a, 0x00, 0x00];
        let bits = [0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0];
        for (i, &b) in bits.iter().enumerate() {
            if b == 0 {
                payload.push(i as u8);
            } else {
                payload.extend_from_slice(&[i as u8, 0xa0, 0xb0, 0xc0]);
            }
        }
        let mbs = decode_vector_chunk(VECTOR_CHUNK_INTRA, &payload, 16).unwrap();
        assert_eq!(mbs.len(), 16);
        for (i, &b) in bits.iter().enumerate() {
            match (b, &mbs[i]) {
                (0, Mb::V1(idx)) => assert_eq!(*idx, i as u8),
                (1, Mb::V4(idx)) => assert_eq!(idx[0], i as u8),
                _ => panic!("mismatch at {i}: {:?}", mbs[i]),
            }
        }
    }

    /// Spec §3.3 fixture `Y8` — 16-MB strip, single V1 update + 15
    /// skips, payload 5 bytes (one flag word + one V1 byte).
    #[test]
    fn decodes_inter_y8() {
        // bit 31 = `1` → leading `1`; bit 30 = `0` → V1 selector → V1 mb 0
        // bits 29..0 = `0` × 30 → 15 SKIPs (15 zeroes consume 15 bits;
        // remaining 15 bits are unused padding because mb_count = 16).
        let payload = vec![0x80, 0x00, 0x00, 0x00, 0x42];
        let mbs = decode_vector_chunk(VECTOR_CHUNK_INTER, &payload, 16).unwrap();
        assert_eq!(mbs.len(), 16);
        assert_eq!(mbs[0], Mb::V1(0x42));
        for m in &mbs[1..] {
            assert_eq!(*m, Mb::Skip);
        }
    }

    /// Round-trip: synthesise an arbitrary mixed-intra mb list, encode,
    /// decode, and ensure equality.
    #[test]
    fn roundtrip_mixed_intra() {
        let mbs = vec![
            Mb::V1(10),
            Mb::V4([1, 2, 3, 4]),
            Mb::V1(20),
            Mb::V4([5, 6, 7, 8]),
        ];
        let mut buf = Vec::new();
        encode_mixed_intra_payload(&mbs, &mut buf).unwrap();
        let out = decode_vector_chunk(VECTOR_CHUNK_INTRA, &buf, mbs.len()).unwrap();
        assert_eq!(out, mbs);
    }

    /// Round-trip: V1-only chunk.
    #[test]
    fn roundtrip_v1_only() {
        let mbs: Vec<Mb> = (0..16).map(|i| Mb::V1(i as u8)).collect();
        let mut buf = Vec::new();
        encode_v1_only_payload(&mbs, &mut buf).unwrap();
        let out = decode_vector_chunk(VECTOR_CHUNK_V1_ONLY, &buf, mbs.len()).unwrap();
        assert_eq!(out, mbs);
    }

    /// Round-trip: inter chunk with skips, V1, V4.
    #[test]
    fn roundtrip_inter_simple() {
        let mbs = vec![
            Mb::V1(0x42),
            Mb::Skip,
            Mb::Skip,
            Mb::V4([1, 2, 3, 4]),
            Mb::Skip,
        ];
        let mut buf = Vec::new();
        encode_inter_payload(&mbs, &mut buf).unwrap();
        let out = decode_vector_chunk(VECTOR_CHUNK_INTER, &buf, mbs.len()).unwrap();
        assert_eq!(out, mbs);
    }

    /// Round-trip: 64-MB inter chunk with mixed codes — exercises the
    /// flag-word boundary logic and selector spillover.
    #[test]
    fn roundtrip_inter_64mb() {
        let mut mbs = Vec::new();
        for i in 0..64 {
            mbs.push(match i % 5 {
                0 => Mb::Skip,
                1 => Mb::V1(i as u8),
                2 => Mb::V4([i as u8, i as u8 + 1, i as u8 + 2, i as u8 + 3]),
                3 => Mb::Skip,
                4 => Mb::V1((i * 2) as u8),
                _ => unreachable!(),
            });
        }
        let mut buf = Vec::new();
        encode_inter_payload(&mbs, &mut buf).unwrap();
        let out = decode_vector_chunk(VECTOR_CHUNK_INTER, &buf, mbs.len()).unwrap();
        assert_eq!(out, mbs);
    }

    /// Inter all-skip: 32 SKIP codes fit in one flag word, no index
    /// bytes follow.
    #[test]
    fn roundtrip_inter_all_skip_32() {
        let mbs = vec![Mb::Skip; 32];
        let mut buf = Vec::new();
        encode_inter_payload(&mbs, &mut buf).unwrap();
        // Just one 4-byte flag word, no index data.
        assert_eq!(buf.len(), 4);
        let out = decode_vector_chunk(VECTOR_CHUNK_INTER, &buf, 32).unwrap();
        assert_eq!(out, mbs);
    }

    // ---------- V1OnlyMacroblocks iterator (r246) ----------------------

    /// Spec §3.1 — V1-only chunk payload is exactly `mb_count` bytes;
    /// the iterator yields each codebook index in scan order.
    #[test]
    fn v1only_iter_yields_indices_in_scan_order() {
        let payload: Vec<u8> = (0..16).collect();
        let it = V1OnlyMacroblocks::new(&payload, 16).unwrap();
        let entries: Vec<_> = it.collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(entries.len(), 16);
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.index, i as u16);
            assert_eq!(e.codebook_index, i as u8);
        }
    }

    /// Spec §3.1 fixture `Y6` worked example — single 16-MB strip,
    /// payload byte-per-MB with per-MB-distinct codebook indices.
    #[test]
    fn v1only_iter_y6_fixture() {
        // Y6 payload pattern: indices 0..16 in row-major scan order.
        let payload: Vec<u8> = (0..16).collect();
        let it = V1OnlyMacroblocks::new(&payload, 16).unwrap();
        assert_eq!(it.cursor(), 0);
        assert_eq!(it.mb_count(), 16);
        assert_eq!(it.payload(), &payload[..]);

        let mut it = V1OnlyMacroblocks::new(&payload, 16).unwrap();
        let first = it.next().unwrap().unwrap();
        assert_eq!(first.index, 0);
        assert_eq!(first.codebook_index, 0);
        assert_eq!(it.cursor(), 1);
    }

    /// Spec §3.1 — empty strip (mb_count = 0) is a happy path with
    /// zero yields.
    #[test]
    fn v1only_iter_empty_strip() {
        let payload: Vec<u8> = Vec::new();
        let mut it = V1OnlyMacroblocks::new(&payload, 0).unwrap();
        assert!(it.next().is_none());
        // Still none on subsequent calls.
        assert!(it.next().is_none());
    }

    /// Spec §3.1 — `payload.len() != mb_count` (payload short) is
    /// rejected at construction.
    #[test]
    fn v1only_iter_rejects_payload_short() {
        let payload: Vec<u8> = vec![0, 1, 2]; // 3 bytes
        let err = V1OnlyMacroblocks::new(&payload, 8).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("0x3200") && msg.contains("payload"),
            "unexpected error message: {msg}"
        );
    }

    /// Spec §3.1 — `payload.len() != mb_count` (payload long) is
    /// rejected at construction.
    #[test]
    fn v1only_iter_rejects_payload_long() {
        let payload: Vec<u8> = (0..20).collect(); // 20 bytes for 16 mbs
        let err = V1OnlyMacroblocks::new(&payload, 16).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("0x3200") && msg.contains("payload"),
            "unexpected error message: {msg}"
        );
    }

    /// `size_hint` reports the exact remaining count at every step.
    #[test]
    fn v1only_iter_size_hint_is_exact() {
        let payload: Vec<u8> = (0..16).collect();
        let mut it = V1OnlyMacroblocks::new(&payload, 16).unwrap();
        assert_eq!(it.size_hint(), (16, Some(16)));
        let _ = it.next().unwrap().unwrap();
        assert_eq!(it.size_hint(), (15, Some(15)));
        for _ in 0..15 {
            let _ = it.next().unwrap().unwrap();
        }
        assert_eq!(it.size_hint(), (0, Some(0)));
        assert!(it.next().is_none());
    }

    /// `ExactSizeIterator` implementation reports the right length.
    #[test]
    fn v1only_iter_exact_size() {
        let payload: Vec<u8> = (0..16).collect();
        let it = V1OnlyMacroblocks::new(&payload, 16).unwrap();
        assert_eq!(it.len(), 16);
    }

    /// `cursor()` and `remaining()` accessors advance in lock-step
    /// with `next()`.
    #[test]
    fn v1only_iter_cursor_and_remaining() {
        let payload: Vec<u8> = (0..8).collect();
        let mut it = V1OnlyMacroblocks::new(&payload, 8).unwrap();
        assert_eq!(it.cursor(), 0);
        assert_eq!(it.remaining(), 8);
        let _ = it.next().unwrap().unwrap();
        assert_eq!(it.cursor(), 1);
        assert_eq!(it.remaining(), 7);
        let _ = it.next().unwrap().unwrap();
        let _ = it.next().unwrap().unwrap();
        assert_eq!(it.cursor(), 3);
        assert_eq!(it.remaining(), 5);
    }

    /// Cross-check against `decode_vector_chunk(0x3200, …)`: the
    /// iterator yields the same codebook indices as the bulk decoder.
    #[test]
    fn v1only_iter_matches_decode_vector_chunk() {
        let payload: Vec<u8> = vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80];
        let bulk = decode_vector_chunk(VECTOR_CHUNK_V1_ONLY, &payload, 8).unwrap();
        let iter: Vec<_> = V1OnlyMacroblocks::new(&payload, 8)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(bulk.len(), iter.len());
        for (i, (b, e)) in bulk.iter().zip(iter.iter()).enumerate() {
            match b {
                Mb::V1(idx) => assert_eq!(*idx, e.codebook_index, "mismatch at {i}"),
                _ => panic!("expected V1 for 0x3200 chunk at {i}, got {:?}", b),
            }
        }
    }

    /// Spec §3.1 fixture `Y11` — 64-MB strip → 64-byte payload.
    #[test]
    fn v1only_iter_y11_fixture_64mb() {
        // Y11 reference: 32×32 frame → 64 macroblocks → 64-byte payload.
        let payload: Vec<u8> = (0..64).collect();
        let entries: Vec<_> = V1OnlyMacroblocks::new(&payload, 64)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 64);
        // Last entry's index matches the spec §1.1 row-major scan
        // order: index 63 maps to (row=15, col=3) for a 4-MB-wide
        // strip, but the index field itself is just `MB_count - 1`.
        assert_eq!(entries[63].index, 63);
        assert_eq!(entries[63].codebook_index, 63);
    }

    // ---------- MixedIntraMacroblocks iterator (r250) ------------------

    /// Spec §3.2 fixture `Y9` — 16-MB strip, every macroblock V4. Top
    /// 16 bits of the flag word are `0xffff` (set), low 16 unused.
    /// The iterator yields 16 V4-classified entries in scan order.
    #[test]
    fn mixed_intra_iter_y9_all_v4() {
        let mut payload = vec![0xff, 0xff, 0x00, 0x00];
        for i in 0..16 {
            payload.extend_from_slice(&[i as u8, i as u8 + 1, i as u8 + 2, i as u8 + 3]);
        }
        let entries: Vec<_> = MixedIntraMacroblocks::new(&payload, 16)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 16);
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.index, i as u16);
            assert_eq!(
                e.kind,
                MixedIntraMb::V4([i as u8, i as u8 + 1, i as u8 + 2, i as u8 + 3]),
            );
        }
    }

    /// Spec §3.2 fixture `Y12` — 16-MB strip, checkerboard V1/V4 with
    /// flag-word top 16 bits = `0x5a5a`. 8 V1 + 8 V4 in row-major
    /// scan order.
    #[test]
    fn mixed_intra_iter_y12_checkerboard() {
        let mut payload = vec![0x5a, 0x5a, 0x00, 0x00];
        let bits = [0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0];
        for (i, &b) in bits.iter().enumerate() {
            if b == 0 {
                payload.push(i as u8);
            } else {
                payload.extend_from_slice(&[i as u8, 0xa0, 0xb0, 0xc0]);
            }
        }
        let entries: Vec<_> = MixedIntraMacroblocks::new(&payload, 16)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 16);
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.index, i as u16);
            match (bits[i], e.kind) {
                (0, MixedIntraMb::V1(idx)) => assert_eq!(idx, i as u8),
                (1, MixedIntraMb::V4(idx)) => assert_eq!(idx, [i as u8, 0xa0, 0xb0, 0xc0]),
                _ => panic!("mismatch at {i}: {:?}", e.kind),
            }
        }
    }

    /// Spec §3.2 fixture `Y14` — 64-MB strip, every macroblock V4.
    /// Two flag words of `0xffffffff`, 32 V4 entries per group.
    /// Exercises the group-refill path.
    #[test]
    fn mixed_intra_iter_y14_two_groups_all_v4() {
        let mut payload = vec![0xff, 0xff, 0xff, 0xff];
        for i in 0..32u8 {
            payload.extend_from_slice(&[
                i,
                i.wrapping_add(1),
                i.wrapping_add(2),
                i.wrapping_add(3),
            ]);
        }
        payload.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        for i in 32..64u8 {
            payload.extend_from_slice(&[
                i,
                i.wrapping_add(1),
                i.wrapping_add(2),
                i.wrapping_add(3),
            ]);
        }
        let entries: Vec<_> = MixedIntraMacroblocks::new(&payload, 64)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 64);
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.index, i as u16);
            let b = i as u8;
            assert_eq!(
                e.kind,
                MixedIntraMb::V4([b, b.wrapping_add(1), b.wrapping_add(2), b.wrapping_add(3)]),
            );
        }
    }

    /// All-V1 mixed-intra 16-MB strip: flag word `0x00000000`, 16
    /// V1 index bytes follow. Payload `4 + 16 = 20` bytes.
    #[test]
    fn mixed_intra_iter_all_v1() {
        let mut payload = vec![0x00, 0x00, 0x00, 0x00];
        payload.extend_from_slice(&(0u8..16).collect::<Vec<_>>());
        let entries: Vec<_> = MixedIntraMacroblocks::new(&payload, 16)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 16);
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.index, i as u16);
            assert_eq!(e.kind, MixedIntraMb::V1(i as u8));
        }
    }

    /// Empty strip (`mb_count == 0`) is the trivial happy path: no
    /// flag word is read, the iterator yields nothing.
    #[test]
    fn mixed_intra_iter_empty_strip() {
        let payload: Vec<u8> = Vec::new();
        let mut it = MixedIntraMacroblocks::new(&payload, 0);
        assert!(it.next().is_none());
        assert!(it.next().is_none());
        assert_eq!(it.cursor(), 0);
        assert_eq!(it.remaining(), 0);
        assert_eq!(it.mb_count(), 0);
    }

    /// Truncated flag word (only 3 bytes of payload) is reported on
    /// the first `next()` and the iterator fuses afterwards.
    #[test]
    fn mixed_intra_iter_truncated_flag_word_fuses() {
        let payload = vec![0xff, 0xff, 0xff]; // 3 bytes — short of 4-byte flag word
        let mut it = MixedIntraMacroblocks::new(&payload, 1);
        let err = it.next().unwrap().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("0x3000") && msg.contains("flag-word"),
            "unexpected error message: {msg}"
        );
        assert!(it.next().is_none(), "iterator must fuse on first error");
        assert!(it.next().is_none(), "fuse must hold across calls");
    }

    /// Truncated V1 index byte after the flag word is reported and
    /// the iterator fuses.
    #[test]
    fn mixed_intra_iter_truncated_v1_index_fuses() {
        // Flag word 0x00000000 ⇒ all V1; but provide zero V1 index
        // bytes after the flag word. mb_count = 1 ⇒ 1 V1 index byte
        // is expected; payload is short.
        let payload = vec![0x00, 0x00, 0x00, 0x00];
        let mut it = MixedIntraMacroblocks::new(&payload, 1);
        let err = it.next().unwrap().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("0x3000") && msg.contains("V1-index"),
            "unexpected error message: {msg}"
        );
        assert!(it.next().is_none());
    }

    /// Truncated V4 index quadruple after the flag word is reported
    /// and the iterator fuses.
    #[test]
    fn mixed_intra_iter_truncated_v4_index_fuses() {
        // Flag word top bit set ⇒ MB 0 is V4; provide 0 of the 4
        // expected index bytes.
        let payload = vec![0x80, 0x00, 0x00, 0x00];
        let mut it = MixedIntraMacroblocks::new(&payload, 1);
        let err = it.next().unwrap().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("0x3000") && msg.contains("V4-index"),
            "unexpected error message: {msg}"
        );
        assert!(it.next().is_none());
    }

    /// `size_hint` reports `(remaining, Some(remaining))` based on
    /// the configured `mb_count` and advances in lock-step with
    /// `next()`.
    #[test]
    fn mixed_intra_iter_size_hint_tracks_mb_count() {
        let mut payload = vec![0x00, 0x00, 0x00, 0x00];
        payload.extend_from_slice(&(0u8..16).collect::<Vec<_>>());
        let mut it = MixedIntraMacroblocks::new(&payload, 16);
        assert_eq!(it.size_hint(), (16, Some(16)));
        let _ = it.next().unwrap().unwrap();
        assert_eq!(it.size_hint(), (15, Some(15)));
        for _ in 0..15 {
            let _ = it.next().unwrap().unwrap();
        }
        assert_eq!(it.size_hint(), (0, Some(0)));
        assert!(it.next().is_none());
    }

    /// `cursor` and `remaining` accessors advance after each yield.
    /// Mixed payload of 4-byte flag + V1 (1B) + V4 (4B) + V1 (1B) + V1
    /// (1B) for the first 4 MBs of a 4-MB strip.
    #[test]
    fn mixed_intra_iter_cursor_advances() {
        // Flag word: bit 31=0 (V1), bit 30=1 (V4), bit 29=0 (V1),
        // bit 28=0 (V1) ⇒ top 4 bits = 0b0100 ⇒ flag word
        // 0x40000000.
        let payload = vec![
            0x40, 0x00, 0x00, 0x00, //
            0xaa, // MB0 V1 idx
            0x01, 0x02, 0x03, 0x04, // MB1 V4 idx
            0xbb, // MB2 V1 idx
            0xcc, // MB3 V1 idx
        ];
        let mut it = MixedIntraMacroblocks::new(&payload, 4);
        assert_eq!(it.cursor(), 0);
        assert_eq!(it.remaining(), 4);
        let e0 = it.next().unwrap().unwrap();
        assert_eq!(e0.kind, MixedIntraMb::V1(0xaa));
        // cursor advanced 4 (flag) + 1 (V1) = 5
        assert_eq!(it.cursor(), 5);
        assert_eq!(it.remaining(), 3);
        let e1 = it.next().unwrap().unwrap();
        assert_eq!(e1.kind, MixedIntraMb::V4([0x01, 0x02, 0x03, 0x04]));
        assert_eq!(it.cursor(), 9);
        let e2 = it.next().unwrap().unwrap();
        assert_eq!(e2.kind, MixedIntraMb::V1(0xbb));
        let e3 = it.next().unwrap().unwrap();
        assert_eq!(e3.kind, MixedIntraMb::V1(0xcc));
        assert!(it.next().is_none());
        assert_eq!(it.cursor(), 11);
        assert_eq!(it.remaining(), 0);
    }

    /// Cross-check against `decode_vector_chunk(0x3000, …)`: for the
    /// `Y12` checkerboard fixture both APIs yield equivalent
    /// per-macroblock classifications and index bytes.
    #[test]
    fn mixed_intra_iter_matches_decode_vector_chunk() {
        // Reuse Y12 payload from above.
        let mut payload = vec![0x5a, 0x5a, 0x00, 0x00];
        let bits = [0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0];
        for (i, &b) in bits.iter().enumerate() {
            if b == 0 {
                payload.push(i as u8);
            } else {
                payload.extend_from_slice(&[i as u8, 0xa0, 0xb0, 0xc0]);
            }
        }
        let bulk = decode_vector_chunk(VECTOR_CHUNK_INTRA, &payload, 16).unwrap();
        let typed: Vec<_> = MixedIntraMacroblocks::new(&payload, 16)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(bulk.len(), typed.len());
        for (i, (b, e)) in bulk.iter().zip(typed.iter()).enumerate() {
            assert_eq!(e.index, i as u16, "index mismatch at {i}");
            match (b, e.kind) {
                (Mb::V1(bx), MixedIntraMb::V1(ex)) => assert_eq!(*bx, ex, "V1 idx at {i}"),
                (Mb::V4(bx), MixedIntraMb::V4(ex)) => assert_eq!(*bx, ex, "V4 idx at {i}"),
                _ => panic!("kind mismatch at {i}: bulk {:?}, typed {:?}", b, e.kind),
            }
        }
    }

    /// Group-boundary stress: a 33-MB strip forces a second flag word
    /// after exactly 32 macroblocks. Mixing V1 / V4 around the
    /// boundary exercises the refill path.
    #[test]
    fn mixed_intra_iter_group_boundary_at_32() {
        // Group 0: 32 MBs, all V1 (flag word 0x00000000), 32 idx
        // bytes follow.
        // Group 1: 1 MB, V4 (flag word top bit 1, rest don't care),
        // 4 idx bytes follow.
        let mut payload = vec![0x00, 0x00, 0x00, 0x00];
        payload.extend_from_slice(&(0u8..32).collect::<Vec<_>>());
        payload.extend_from_slice(&[0x80, 0x00, 0x00, 0x00]);
        payload.extend_from_slice(&[0x10, 0x20, 0x30, 0x40]);
        let entries: Vec<_> = MixedIntraMacroblocks::new(&payload, 33)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 33);
        for (i, entry) in entries.iter().enumerate().take(32) {
            assert_eq!(entry.kind, MixedIntraMb::V1(i as u8));
        }
        assert_eq!(entries[32].kind, MixedIntraMb::V4([0x10, 0x20, 0x30, 0x40]));
    }
}
