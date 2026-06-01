//! Round-202 seed generator for the three fuzz targets that landed
//! without committed corpora (`codebook_chunk_apply`,
//! `decode_deviant_frame`, `decode_vector_chunk`). Mirrors the
//! round-196 seeding pattern that primed `decode_multi_frame` with
//! encoder-roundtrippable streams under named filenames so the
//! libFuzzer mutator starts from structurally-valid inputs instead
//! of having to first synthesise a well-formed header from scratch.
//!
//! Per `docs/video/cinepak/spec/` each fuzz target consumes a
//! different framing on top of the raw input bytes:
//!
//! - `codebook_chunk_apply` — first 2 bytes are the BE wire chunk-id,
//!   3rd byte's LSB picks `tolerate_trailing`, rest is the codebook
//!   payload (chunk header bytes excluded). See `fuzz_targets/
//!   codebook_chunk_apply.rs` for the harness wiring.
//! - `decode_vector_chunk` — first 2 bytes are the BE vector chunk-id,
//!   bytes 2..4 are BE `mb_count`, rest is the vector payload.
//! - `decode_deviant_frame` — the raw bytes ARE the deviant frame in
//!   its entirety (10-byte standard frame header + extra header
//!   bytes + strip header + chunk stream); the harness peeks
//!   width/height at offsets 4..8 for the OOM cap then drives
//!   `decode_deviant_frame` against the three documented
//!   `DeviantConfig` permutations.
//!
//! Usage:
//!
//!     cargo run --example seed_fuzz_corpora --release
//!
//! Writes named seed files into `fuzz/corpus/<target>/` underneath
//! the current crate. Existing files with the same names are
//! overwritten; libFuzzer-discovered hash-named corpus units are
//! left alone (the named seeds and the SHA-named units coexist).
//! Seeds are deterministic — re-running the generator produces
//! byte-identical files.

use std::fs;
use std::path::PathBuf;

use oxideav_cinepak::codebook::{
    encode_full_chunk, encode_header_only_chunk, encode_selective_chunk, Codebook,
    CodebookChunkKind, CodebookEntry, UpdateStyle,
};
use oxideav_cinepak::vector::{
    encode_inter_payload, encode_mixed_intra_payload, encode_v1_only_payload, Mb,
};

fn main() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let corpus_root = crate_root.join("fuzz").join("corpus");

    let mut wrote = 0usize;

    // ---- codebook_chunk_apply ----
    let cb_dir = corpus_root.join("codebook_chunk_apply");
    fs::create_dir_all(&cb_dir).expect("mkdir codebook_chunk_apply");
    for (name, bytes) in build_codebook_seeds() {
        let path = cb_dir.join(&name);
        fs::write(&path, &bytes).expect("write codebook seed");
        println!("codebook_chunk_apply/{:<40} {:>5} B", name, bytes.len());
        wrote += 1;
    }

    // ---- decode_vector_chunk ----
    let vc_dir = corpus_root.join("decode_vector_chunk");
    fs::create_dir_all(&vc_dir).expect("mkdir decode_vector_chunk");
    for (name, bytes) in build_vector_seeds() {
        let path = vc_dir.join(&name);
        fs::write(&path, &bytes).expect("write vector seed");
        println!("decode_vector_chunk/{:<38} {:>5} B", name, bytes.len());
        wrote += 1;
    }

    // ---- decode_deviant_frame ----
    let dv_dir = corpus_root.join("decode_deviant_frame");
    fs::create_dir_all(&dv_dir).expect("mkdir decode_deviant_frame");
    for (name, bytes) in build_deviant_seeds() {
        let path = dv_dir.join(&name);
        fs::write(&path, &bytes).expect("write deviant seed");
        println!("decode_deviant_frame/{:<37} {:>5} B", name, bytes.len());
        wrote += 1;
    }

    println!(
        "\n{wrote} seed file(s) written under {}",
        corpus_root.display()
    );
}

// ---------------------------------------------------------------------------
// codebook_chunk_apply seeds
//
// Harness wire format: [id_hi, id_lo, tolerate_lsb_byte, payload...].
// We pre-cover all 8 chunk-kind codes plus the deviant `tolerate_trailing`
// branch and a header-only "no payload" seed (the chunk parser's zero-
// length input path).
// ---------------------------------------------------------------------------

fn build_codebook_seeds() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();

    // A small populated codebook for full-replace seeds (8 entries —
    // covers the full-replace stride arithmetic without bloating the
    // seed file unnecessarily; the fuzzer can grow it).
    let mut cb = Codebook::default();
    for i in 0..8 {
        cb.entries[i as usize] = CodebookEntry::from_yuv(
            10 + i * 16,
            20 + i * 16,
            30 + i * 16,
            40 + i * 16,
            (i as i8) * 4,
            -(i as i8) * 4,
        );
    }

    // Each of the 8 codebook chunk-id codes: V4/V1 × Full/Selective ×
    // YUV/Gray. We exercise every leaf so libFuzzer's coverage map
    // sees each parser arm activated on the seed pass.
    let chunk_ids: &[(u16, &str)] = &[
        (0x2000, "v4-full-yuv"),
        (0x2100, "v4-selective-yuv"),
        (0x2200, "v1-full-yuv"),
        (0x2300, "v1-selective-yuv"),
        (0x2400, "v4-full-gray"),
        (0x2500, "v4-selective-gray"),
        (0x2600, "v1-full-gray"),
        (0x2700, "v1-selective-gray"),
    ];

    for (id, label) in chunk_ids {
        let kind = CodebookChunkKind::from_id(*id).unwrap_or_else(|| {
            panic!("chunk id 0x{id:04x} ({label}) should be a valid codebook kind")
        });
        let payload = encode_payload_for_kind(kind, &cb);

        // Strict-mode seed (tolerate_trailing = false).
        let mut bytes = Vec::with_capacity(3 + payload.len());
        bytes.extend_from_slice(&id.to_be_bytes());
        bytes.push(0); // tolerate_trailing = false
        bytes.extend_from_slice(&payload);
        out.push((format!("{label}-strict"), bytes));

        // Tolerant-mode seed (tolerate_trailing = true), with 2 trailing
        // pad bytes appended. Per spec §3.1 of `02-codebooks.md` the
        // trailing-pad tolerance only applies to full-replace chunks
        // (`apply_full` floor-divides `payload_len / entry_size` when
        // `tolerate_trailing == true`); selective-update chunks decode
        // 32-entry flag-word groups and have no analogous slack — pad
        // bytes appended after the last group return
        // `selective-update codebook truncated mid-flag-word` under
        // both strict and tolerant. We still emit the seed for the
        // selective rows so libFuzzer sees the *tolerant flag*
        // toggled across the rejection arm; it just doesn't represent
        // a Saturn-conformant deviant wire shape.
        let mut bytes = Vec::with_capacity(3 + payload.len() + 2);
        bytes.extend_from_slice(&id.to_be_bytes());
        bytes.push(1); // tolerate_trailing = true
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&[0xab, 0xcd]);
        out.push((format!("{label}-tolerant-pad"), bytes));
    }

    // Header-only "no payload" seed for the 0x2000 V4-full path —
    // a header-only chunk is the canonical "inherit previous strip's
    // codebook" wire pattern per spec §3.4 of 02-codebooks.md and the
    // chunk parser must accept a zero-length payload without panicking.
    let kind = CodebookChunkKind::from_id(0x2000).unwrap();
    let mut payload = Vec::new();
    encode_header_only_chunk(kind, &mut payload);
    // encode_header_only_chunk writes the 4-byte header; for the fuzz
    // target we strip the chunk header and keep only the (empty) payload.
    let mut bytes = Vec::with_capacity(3);
    bytes.extend_from_slice(&0x2000u16.to_be_bytes());
    bytes.push(0);
    out.push(("v4-full-yuv-header-only".to_string(), bytes));

    // A truncated-payload seed: claim chunk 0x2000 (V4 full YUV, 6-byte
    // stride) but provide only 5 payload bytes — the strict path must
    // return an error on the divisibility check without panicking. This
    // hands libFuzzer the "stride boundary" pattern explicitly.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0x2000u16.to_be_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&[10, 20, 30, 40, 0]);
    out.push(("v4-full-yuv-truncated-5b".to_string(), bytes));

    out
}

fn encode_payload_for_kind(kind: CodebookChunkKind, cb: &Codebook) -> Vec<u8> {
    let mut wrapped = Vec::new();
    match kind.style {
        UpdateStyle::Full => {
            encode_full_chunk(kind, cb, 8, &mut wrapped);
        }
        UpdateStyle::Selective => {
            // Pick 5 slots distributed across two 32-slot groups so the
            // selective-update flag-word loop runs at least twice.
            let slots: [u8; 5] = [0, 1, 5, 32, 40];
            encode_selective_chunk(kind, cb, &slots, &mut wrapped);
        }
    }
    // Strip the 4-byte chunk header — the fuzz target consumes payload
    // only (no chunk header).
    assert!(wrapped.len() >= 4, "encode_*_chunk output too short");
    wrapped[4..].to_vec()
}

// ---------------------------------------------------------------------------
// decode_vector_chunk seeds
//
// Harness wire format: [id_hi, id_lo, mbcount_hi, mbcount_lo, payload...].
// One seed per documented chunk code plus an inter seed with a known
// flag-word straddle (the bug surface the round-5 selector-spillover
// fix closed).
// ---------------------------------------------------------------------------

fn build_vector_seeds() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();

    // 0x3200 — V1-only intra. 4 macroblocks (one 8×8 frame's worth at
    // 4×4 MB granularity), each pointing at a different codebook slot.
    let mbs = vec![Mb::V1(0), Mb::V1(1), Mb::V1(2), Mb::V1(3)];
    let mut payload = Vec::new();
    encode_v1_only_payload(&mbs, &mut payload).expect("encode_v1_only_payload");
    out.push((
        "v1-only-4mb".to_string(),
        wrap_vector_seed(0x3200, mbs.len(), &payload),
    ));

    // 0x3000 — mixed V1/V4 intra. Mix V4 + V1 in the first group so the
    // flag-word/index-byte arithmetic both branches are exercised.
    let mbs = vec![
        Mb::V4([0, 1, 2, 3]),
        Mb::V1(4),
        Mb::V4([5, 6, 7, 8]),
        Mb::V1(9),
    ];
    let mut payload = Vec::new();
    encode_mixed_intra_payload(&mbs, &mut payload).expect("encode_mixed_intra_payload");
    out.push((
        "mixed-intra-4mb-alternating".to_string(),
        wrap_vector_seed(0x3000, mbs.len(), &payload),
    ));

    // 0x3000 — mixed V1/V4 intra spanning two flag-word groups (33 MBs)
    // so the per-32-MB regroup is exercised at the seed step.
    let mut mbs = Vec::with_capacity(33);
    for i in 0..33u8 {
        if i % 2 == 0 {
            mbs.push(Mb::V1(i));
        } else {
            mbs.push(Mb::V4([
                i,
                i.wrapping_add(1),
                i.wrapping_add(2),
                i.wrapping_add(3),
            ]));
        }
    }
    let mut payload = Vec::new();
    encode_mixed_intra_payload(&mbs, &mut payload).expect("encode_mixed_intra_payload 33mb");
    out.push((
        "mixed-intra-33mb-2-flag-words".to_string(),
        wrap_vector_seed(0x3000, mbs.len(), &payload),
    ));

    // 0x3100 — inter with all SKIPs. The simplest inter wire pattern;
    // every code is a single `0` bit. 32 MBs = exactly one flag word.
    let mbs = vec![Mb::Skip; 32];
    let mut payload = Vec::new();
    encode_inter_payload(&mbs, &mut payload).expect("encode_inter_payload all-skip");
    out.push((
        "inter-all-skip-32mb".to_string(),
        wrap_vector_seed(0x3100, mbs.len(), &payload),
    ));

    // 0x3100 — inter with mixed SKIP/V1/V4 that forces a `1`-bit selector
    // to land at bit 31 of a flag word so the V1/V4 selector spills to
    // the MSB of the next flag word — the exact straddle pattern the
    // round-5 spillover fix covers. Pattern is derived from the
    // `tests/inter_payload_straddle.rs` regression test.
    let mut mbs = vec![Mb::Skip; 31];
    mbs.push(Mb::V4([10, 11, 12, 13])); // emits `11` straddling bit 31..32
    mbs.push(Mb::V1(7));
    mbs.push(Mb::Skip);
    let mut payload = Vec::new();
    encode_inter_payload(&mbs, &mut payload).expect("encode_inter_payload straddle");
    out.push((
        "inter-straddle-34mb-selector-spillover".to_string(),
        wrap_vector_seed(0x3100, mbs.len(), &payload),
    ));

    // Unknown chunk-id: 0x0000. The dispatcher's "unknown vector chunk
    // id" rejection path must not panic. Seed at mb_count = 4 to give
    // libFuzzer a small input to mutate from.
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0u8; 4]);
    out.push((
        "unknown-id-0x0000-4mb".to_string(),
        wrap_vector_seed(0x0000, 4, &payload),
    ));

    out
}

fn wrap_vector_seed(chunk_id: u16, mb_count: usize, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&chunk_id.to_be_bytes());
    let mb_count_u16: u16 = mb_count.try_into().unwrap_or(u16::MAX);
    out.extend_from_slice(&mb_count_u16.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

// ---------------------------------------------------------------------------
// decode_deviant_frame seeds
//
// The harness consumes the raw bytes as the deviant frame in its
// entirety. We emit one Saturn-prefix (2 extra header bytes) seed and
// one Lemmings-3DO-prefix (6 extra header bytes) seed, both with the
// `frame_length`-short-by-8 deviation and a codebook chunk that has 2
// trailing pad bytes (the `tolerate_codebook_pad` branch). Pattern is
// derived from `tests/deviant_saturn.rs::build_deviant_2_extra` —
// authored here from scratch against the same spec source
// (`docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 125–143
// and 189; `docs/video/cinepak/spec/01-frame-and-strip.md`).
// ---------------------------------------------------------------------------

fn build_deviant_seeds() -> Vec<(String, Vec<u8>)> {
    vec![
        (
            "saturn-2extra-4x4-cbpad".to_string(),
            build_deviant_frame(2),
        ),
        (
            "lemmings3do-6extra-4x4-cbpad".to_string(),
            build_deviant_frame(6),
        ),
        // Also emit a standard-control seed under the deviant corpus —
        // the harness exercises the `decode_frame` strict path too, and
        // a well-formed standard frame is a useful coverage anchor for
        // the strict-vs-deviant branch comparison.
        ("standard-control-4x4".to_string(), build_standard_frame()),
    ]
}

fn build_deviant_frame(extra_header_bytes: usize) -> Vec<u8> {
    let mut bytes = Vec::new();

    // 10-byte standard frame header.
    bytes.push(0x00); // flags
    bytes.extend_from_slice(&[0x00, 0x00, 0x00]); // frame_length placeholder
    bytes.extend_from_slice(&4u16.to_be_bytes()); // width
    bytes.extend_from_slice(&4u16.to_be_bytes()); // height
    bytes.extend_from_slice(&1u16.to_be_bytes()); // strip_count

    // Deviant: extra header bytes (2 = Saturn, 6 = Lemmings 3DO).
    bytes.resize(bytes.len() + extra_header_bytes, 0x00);

    // 12-byte strip header.
    let strip_hdr_pos = bytes.len();
    bytes.extend_from_slice(&0x1000u16.to_be_bytes()); // strip_id intra
    bytes.extend_from_slice(&0u16.to_be_bytes()); // strip_size placeholder
    bytes.extend_from_slice(&0u16.to_be_bytes()); // y_top
    bytes.extend_from_slice(&0u16.to_be_bytes()); // x_top
    bytes.extend_from_slice(&4u16.to_be_bytes()); // y_bottom
    bytes.extend_from_slice(&4u16.to_be_bytes()); // x_bottom

    // V4 codebook chunk — header-only ("inherit previous codebook";
    // a 4×4 deviant frame referencing only V1 doesn't need V4
    // entries, and header-only is the minimum the spec accepts).
    bytes.extend_from_slice(&[0x20, 0x00, 0x00, 0x04]);

    // V1 codebook chunk with 1 valid entry + 2 trailing pad bytes
    // (the `tolerate_codebook_pad` deviant branch). chunk_id = 0x2200
    // (V1 full YUV). Entry stride is 6 B; we emit 6 + 2 pad = 8 B
    // payload, so chunk_size = 4 (header) + 8 (payload) = 12 = 0x000c.
    bytes.extend_from_slice(&0x2200u16.to_be_bytes());
    bytes.extend_from_slice(&0x000cu16.to_be_bytes());
    bytes.extend_from_slice(&[50, 100, 150, 200, 0, 0]); // V1[0] entry
    bytes.extend_from_slice(&[0xab, 0xcd]); // 2 trailing pad bytes

    // Vector chunk 0x3200 with 1 MB referencing V1 entry 0.
    bytes.extend_from_slice(&0x3200u16.to_be_bytes());
    bytes.extend_from_slice(&0x0005u16.to_be_bytes()); // chunk_size = 4 + 1
    bytes.push(0x00); // V1 index 0

    // Patch strip_size.
    let strip_size = (bytes.len() - strip_hdr_pos) as u16;
    bytes[strip_hdr_pos + 2..strip_hdr_pos + 4].copy_from_slice(&strip_size.to_be_bytes());

    // Patch frame_length: declared = real - 8 (the deviant undercount).
    let real_len = bytes.len() as u32;
    let declared = real_len - 8;
    bytes[1] = ((declared >> 16) & 0xff) as u8;
    bytes[2] = ((declared >> 8) & 0xff) as u8;
    bytes[3] = (declared & 0xff) as u8;

    bytes
}

fn build_standard_frame() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(0x00); // flags
    bytes.extend_from_slice(&[0x00, 0x00, 0x00]); // frame_length placeholder
    bytes.extend_from_slice(&4u16.to_be_bytes()); // width
    bytes.extend_from_slice(&4u16.to_be_bytes()); // height
    bytes.extend_from_slice(&1u16.to_be_bytes()); // strip_count

    let strip_hdr_pos = bytes.len();
    bytes.extend_from_slice(&0x1000u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&4u16.to_be_bytes());

    // V4 header-only.
    bytes.extend_from_slice(&[0x20, 0x00, 0x00, 0x04]);
    // V1 full, single conformant entry, no pad.
    bytes.extend_from_slice(&0x2200u16.to_be_bytes());
    bytes.extend_from_slice(&0x000au16.to_be_bytes()); // 4 + 6
    bytes.extend_from_slice(&[50, 100, 150, 200, 0, 0]);
    // Vector chunk.
    bytes.extend_from_slice(&0x3200u16.to_be_bytes());
    bytes.extend_from_slice(&0x0005u16.to_be_bytes());
    bytes.push(0x00);

    let strip_size = (bytes.len() - strip_hdr_pos) as u16;
    bytes[strip_hdr_pos + 2..strip_hdr_pos + 4].copy_from_slice(&strip_size.to_be_bytes());

    // Standard path: frame_length = real length (no undercount).
    let real_len = bytes.len() as u32;
    bytes[1] = ((real_len >> 16) & 0xff) as u8;
    bytes[2] = ((real_len >> 8) & 0xff) as u8;
    bytes[3] = (real_len & 0xff) as u8;

    bytes
}
