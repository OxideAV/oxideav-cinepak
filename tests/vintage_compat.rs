//! Vintage decoder compatibility (`EncoderOptions::vintage_compat`).
//!
//! Covers the two `Cinepak.wiki` line 33 structural constraints quoted
//! verbatim in:
//!
//! - `docs/video/cinepak/spec/01-frame-and-strip.md` §2.3 ("Vintage
//!   players on both Windows and MacOS work only if the number of strips
//!   per frame does not exceed 3.").
//! - `docs/video/cinepak/spec/02-codebooks.md` §2.2 + §3.4 ("MacOS
//!   player needs codebook definitions always to be present even for
//!   empty codebooks, in strict order (v4, then v1)" — header-only
//!   chunks satisfy the constraint while still signalling "inherit
//!   previous codebook").
//!
//! When `vintage_compat = true`:
//!
//! 1. `strip_count > 3` is rejected at `validate_opts` time.
//! 2. Inter strips that would otherwise chunk-omit (decoder inherits
//!    previous codebook, 0 wire bytes) fall back to a header-only chunk
//!    (`chunk_size = 0x0004`, 4 wire bytes) so the wire chunk stream
//!    always carries both V4 then V1 codebook chunks per strip.
//! 3. Round-trip decode remains identical (header-only and chunk-omitted
//!    forms both signal codebook inheritance per spec §3.4).

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{
    encode_rgb24, CinepakDecoder, CinepakEncoder, CinepakPixelFormat, EncoderOptions,
};

/// 32×32 RGB fixture: solid mid-gray. Used as both the intra and the
/// follow-up inter frame so the inter-frame codebook can stay
/// identical to the intra's — the chunk-omission path (default) emits
/// nothing on the inter strips, the vintage path emits header-only
/// chunks.
fn solid_gray_32(value: u8) -> Vec<u8> {
    vec![value; 32 * 32 * 3]
}

/// Walk a Cinepak frame and count the codebook chunks (`0x20xx`–`0x27xx`
/// family) emitted per strip, recording each chunk's `(strip_index,
/// chunk_id, chunk_size)`. Also returns the per-strip strip_id so we
/// can filter to inter strips.
fn enumerate_codebook_chunks(bytes: &[u8]) -> (u16, Vec<(usize, u16, u16, u16)>) {
    let strip_count = u16::from_be_bytes([bytes[8], bytes[9]]);
    let mut out = Vec::new();
    let mut off = 10usize; // skip 10-byte frame header
    for s in 0..(strip_count as usize) {
        let strip_id = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
        let strip_size = u16::from_be_bytes([bytes[off + 2], bytes[off + 3]]);
        let strip_end = off + strip_size as usize;
        let mut p = off + 12; // skip 12-byte strip header
        while p + 4 <= strip_end {
            let cid = u16::from_be_bytes([bytes[p], bytes[p + 1]]);
            let csz = u16::from_be_bytes([bytes[p + 2], bytes[p + 3]]);
            // Codebook chunks: 0x2000..=0x2700 family.
            if (0x2000..=0x2700).contains(&cid) && cid & 0x00ff == 0 {
                out.push((s, strip_id, cid, csz));
            }
            p += csz as usize;
        }
        off = strip_end;
    }
    (strip_count, out)
}

/// `vintage_compat = true` + `strip_count = 4` → rejected at
/// `validate_opts` time with a clear message.
#[test]
fn rejects_strip_count_above_three_when_vintage_compat() {
    let rgb = solid_gray_32(128);
    let opts = EncoderOptions {
        strip_count: 4,
        vintage_compat: true,
        ..EncoderOptions::default()
    };
    let r = encode_rgb24(&rgb, 32, 32, opts);
    assert!(r.is_err(), "strip_count=4 with vintage_compat must error");
    let msg = format!("{}", r.err().unwrap());
    assert!(
        msg.contains("vintage_compat") && msg.contains("strip_count"),
        "error message {msg:?} should mention vintage_compat + strip_count"
    );
}

/// `strip_count = 3` is the vintage-compat ceiling — accepted.
#[test]
fn accepts_strip_count_three_when_vintage_compat() {
    let rgb = solid_gray_32(128);
    let opts = EncoderOptions {
        strip_count: 3,
        vintage_compat: true,
        ..EncoderOptions::default()
    };
    let bytes = encode_rgb24(&rgb, 32, 32, opts).expect("strip_count=3 accepted");
    // Decode roundtrip: bytes are well-formed Cinepak.
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).expect("roundtrip decode");
    assert_eq!((f.width, f.height), (32, 32));
    assert_eq!(f.pixel_format, CinepakPixelFormat::Rgb24);
}

/// `strip_count > 3` is fine when `vintage_compat = false` (the default).
/// Sanity check: the new option doesn't accidentally restrict the
/// default path.
#[test]
fn accepts_strip_count_above_three_when_default() {
    let rgb = solid_gray_32(128);
    let opts = EncoderOptions {
        strip_count: 4,
        ..EncoderOptions::default()
    };
    let bytes = encode_rgb24(&rgb, 32, 32, opts).expect("strip_count=4 default-accepted");
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).expect("roundtrip decode");
    assert_eq!((f.width, f.height), (32, 32));
}

/// Default behaviour (`vintage_compat = false`): an inter frame whose
/// codebook is byte-identical to the previous strip's chunk-omits both
/// V4 and V1 codebook chunks per strip — 0 codebook chunks total on the
/// inter strip (spec §3.4 chunk-omission path).
#[test]
fn default_inter_chunk_omission_emits_zero_codebook_chunks() {
    let mut enc = CinepakEncoder::new();
    let opts = EncoderOptions {
        strip_count: 1,
        v4_entries: 16,
        v1_entries: 16,
        ..EncoderOptions::default()
    };
    let rgb = solid_gray_32(128);
    let _intra = enc.encode_intra(&rgb, 32, 32, opts).expect("intra encode");
    // Inter frame: same pixels → the codebook trained for this strip
    // matches the prior strip's, so the chunk-omission path fires.
    let inter = enc.encode_inter(&rgb, 32, 32, opts).expect("inter encode");
    let (strip_count, chunks) = enumerate_codebook_chunks(&inter);
    assert_eq!(strip_count, 1);
    let inter_codebooks: Vec<_> = chunks
        .iter()
        .filter(|(_, sid, _, _)| *sid == 0x1100)
        .collect();
    assert_eq!(
        inter_codebooks.len(),
        0,
        "default path: chunk-omitted inter strip carries no codebook chunks, got {inter_codebooks:?}"
    );
}

/// `vintage_compat = true`: the same identical-codebook inter strip
/// emits a header-only `0x2000` (V4) and `0x2200` (V1) chunk, in strict
/// V4-then-V1 order, each with `chunk_size = 0x0004`. The wire chunk
/// stream then carries the structurally-required pair vintage MacOS
/// decoders insist on (`Cinepak.wiki` line 33).
#[test]
fn vintage_compat_inter_emits_header_only_chunks_in_v4_then_v1_order() {
    let mut enc = CinepakEncoder::new();
    let opts = EncoderOptions {
        strip_count: 1,
        v4_entries: 16,
        v1_entries: 16,
        vintage_compat: true,
        ..EncoderOptions::default()
    };
    let rgb = solid_gray_32(128);
    let _intra = enc.encode_intra(&rgb, 32, 32, opts).expect("intra encode");
    let inter = enc.encode_inter(&rgb, 32, 32, opts).expect("inter encode");
    let (_, chunks) = enumerate_codebook_chunks(&inter);
    let inter_codebooks: Vec<_> = chunks
        .iter()
        .filter(|(_, sid, _, _)| *sid == 0x1100)
        .collect();
    // Exactly two header-only codebook chunks: V4 then V1, both
    // chunk_size = 4.
    assert_eq!(
        inter_codebooks.len(),
        2,
        "vintage path: inter strip carries exactly two codebook chunks, got {inter_codebooks:?}"
    );
    let (_, _, cid0, csz0) = *inter_codebooks[0];
    let (_, _, cid1, csz1) = *inter_codebooks[1];
    // V4 first (chunk-id 0x2000 = full-replace 12-bit-YUV V4).
    assert_eq!(cid0, 0x2000, "first chunk must be V4 full-replace");
    // V1 second (chunk-id 0x2200).
    assert_eq!(cid1, 0x2200, "second chunk must be V1 full-replace");
    // Both header-only (chunk_size = 4 = header alone, no payload).
    assert_eq!(csz0, 4, "V4 chunk must be header-only");
    assert_eq!(csz1, 4, "V1 chunk must be header-only");
}

/// Wire-size sanity: the vintage-compat path is at most a small
/// constant (8 bytes per chunk-omitted strip) larger than the default
/// path, since each otherwise-omitted chunk grows from 0 to 4 bytes
/// (and there are two chunks per strip: V4 + V1).
#[test]
fn vintage_compat_wire_size_overhead_is_bounded() {
    let mut enc_default = CinepakEncoder::new();
    let mut enc_vintage = CinepakEncoder::new();
    let opts_default = EncoderOptions {
        strip_count: 2,
        v4_entries: 16,
        v1_entries: 16,
        ..EncoderOptions::default()
    };
    let opts_vintage = EncoderOptions {
        vintage_compat: true,
        ..opts_default
    };
    let rgb = solid_gray_32(128);
    let _ = enc_default
        .encode_intra(&rgb, 32, 32, opts_default)
        .expect("intra default");
    let _ = enc_vintage
        .encode_intra(&rgb, 32, 32, opts_vintage)
        .expect("intra vintage");
    let inter_default = enc_default
        .encode_inter(&rgb, 32, 32, opts_default)
        .expect("inter default");
    let inter_vintage = enc_vintage
        .encode_inter(&rgb, 32, 32, opts_vintage)
        .expect("inter vintage");
    // Each strip can grow by at most 2 chunks × 4 bytes = 8 bytes.
    let max_growth = (opts_default.strip_count as usize) * 8;
    let delta = inter_vintage.len().saturating_sub(inter_default.len());
    assert!(
        delta <= max_growth,
        "vintage_compat overhead {delta} B exceeds {max_growth} B cap on {} strips",
        opts_default.strip_count
    );
}

/// Decode equivalence: a vintage-compat encoded inter frame and a
/// default encoded inter frame produce **byte-identical** decoded
/// pixels (the header-only chunk and the chunk-omission both signal
/// codebook inheritance per spec §3.4 — decoder semantics are
/// identical).
#[test]
fn vintage_compat_decode_matches_default() {
    let mut enc_default = CinepakEncoder::new();
    let mut enc_vintage = CinepakEncoder::new();
    let opts_default = EncoderOptions {
        strip_count: 2,
        v4_entries: 16,
        v1_entries: 16,
        ..EncoderOptions::default()
    };
    let opts_vintage = EncoderOptions {
        vintage_compat: true,
        ..opts_default
    };
    let rgb = solid_gray_32(140);

    let _ = enc_default
        .encode_intra(&rgb, 32, 32, opts_default)
        .expect("intra default");
    let _ = enc_vintage
        .encode_intra(&rgb, 32, 32, opts_vintage)
        .expect("intra vintage");
    let inter_default = enc_default
        .encode_inter(&rgb, 32, 32, opts_default)
        .expect("inter default");
    let inter_vintage = enc_vintage
        .encode_inter(&rgb, 32, 32, opts_vintage)
        .expect("inter vintage");

    // Decode each through a fresh decoder. The previous frame must be
    // seeded by feeding the intra first.
    let mut dec_default = CinepakDecoder::new();
    let mut dec_vintage = CinepakDecoder::new();
    let intra_d = encode_rgb24(&rgb, 32, 32, opts_default).expect("intra repro default");
    let intra_v = encode_rgb24(&rgb, 32, 32, opts_vintage).expect("intra repro vintage");
    let _ = dec_default
        .decode_frame(&intra_d, None)
        .expect("decode intra default");
    let _ = dec_vintage
        .decode_frame(&intra_v, None)
        .expect("decode intra vintage");
    let f_default = dec_default
        .decode_frame(&inter_default, None)
        .expect("decode inter default");
    let f_vintage = dec_vintage
        .decode_frame(&inter_vintage, None)
        .expect("decode inter vintage");
    assert_eq!(f_default.width, f_vintage.width);
    assert_eq!(f_default.height, f_vintage.height);
    assert_eq!(f_default.stride(), f_vintage.stride());
    assert_eq!(
        f_default.pixels(),
        f_vintage.pixels(),
        "vintage-compat and default decode to byte-identical pixels"
    );
}

/// The intra path is already conformant — it always emits V4-then-V1
/// full-replace chunks per strip, irrespective of `vintage_compat`. The
/// new flag is therefore a no-op on intra-only sequences (wire bytes
/// must be byte-identical).
#[test]
fn vintage_compat_is_noop_on_intra() {
    let rgb = solid_gray_32(96);
    let opts_default = EncoderOptions {
        strip_count: 3,
        v4_entries: 32,
        v1_entries: 32,
        ..EncoderOptions::default()
    };
    let opts_vintage = EncoderOptions {
        vintage_compat: true,
        ..opts_default
    };
    let bytes_default = encode_rgb24(&rgb, 32, 32, opts_default).expect("intra default");
    let bytes_vintage = encode_rgb24(&rgb, 32, 32, opts_vintage).expect("intra vintage");
    assert_eq!(
        bytes_default, bytes_vintage,
        "vintage_compat must not change intra-only output bytes"
    );
}

/// Within a multi-strip vintage frame, every inter strip carries
/// **exactly two** codebook chunks (V4 then V1) — none can be omitted.
/// Mixes selective-update with header-only emission paths.
#[test]
fn vintage_compat_every_inter_strip_has_two_codebook_chunks() {
    let mut enc = CinepakEncoder::new();
    let opts = EncoderOptions {
        strip_count: 2,
        v4_entries: 16,
        v1_entries: 16,
        vintage_compat: true,
        ..EncoderOptions::default()
    };
    let rgb = solid_gray_32(160);
    let _ = enc.encode_intra(&rgb, 32, 32, opts).expect("intra");
    let inter = enc.encode_inter(&rgb, 32, 32, opts).expect("inter");
    let (strip_count, chunks) = enumerate_codebook_chunks(&inter);
    assert_eq!(strip_count, 2);
    for s in 0..(strip_count as usize) {
        let per_strip: Vec<_> = chunks.iter().filter(|(idx, _, _, _)| *idx == s).collect();
        assert_eq!(
            per_strip.len(),
            2,
            "strip {s} must carry exactly 2 codebook chunks under vintage_compat, got {per_strip:?}"
        );
        // First V4 then V1, by chunk-id bit-1 selector.
        let (_, _, cid0, _) = *per_strip[0];
        let (_, _, cid1, _) = *per_strip[1];
        assert_eq!(
            cid0 & 0x0200,
            0,
            "strip {s} chunk 0 must be V4 (bit-1 clear), got {cid0:#06x}"
        );
        assert_eq!(
            cid1 & 0x0200,
            0x0200,
            "strip {s} chunk 1 must be V1 (bit-1 set), got {cid1:#06x}"
        );
    }
}
