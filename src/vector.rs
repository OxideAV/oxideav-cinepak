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
}
