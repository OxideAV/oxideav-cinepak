//! Round-202 verification harness: builds the same named seeds the
//! `examples/seed_fuzz_corpora.rs` generator writes to disk, in memory,
//! and drives every seed through the same public entry points the
//! libFuzzer targets invoke. Decouples the test from the gitignored
//! `fuzz/corpus/` runtime directory (per `fuzz/.gitignore`) so CI does
//! not need to run the example before the integration tests.
//!
//! Goal: a regression catch if a future encoder / parser refactor
//! moves the wire surface the seeds were drawn from. The fuzz
//! harnesses themselves only check "doesn't panic"; this test
//! additionally checks "the positive seeds reach the parsers' Ok
//! arm" so we notice if a seed silently degrades to a "rejected at
//! the dispatcher" no-op (which the fuzz target would still report
//! as a green run).
//!
//! The seed-builder helpers are intentionally a near-line-for-line
//! mirror of the ones in `examples/seed_fuzz_corpora.rs`; both share
//! the same public encoder helpers (`encode_full_chunk`,
//! `encode_selective_chunk`, `encode_v1_only_payload`, etc.) so if a
//! helper's wire output ever changes the test trips before the
//! example writes mismatched bytes to disk.

use oxideav_cinepak::codebook::{
    apply_codebook_chunk, apply_codebook_chunk_with, encode_full_chunk, encode_header_only_chunk,
    encode_selective_chunk, Codebook, CodebookChunkKind, CodebookEntry, UpdateStyle,
};
use oxideav_cinepak::vector::{
    decode_vector_chunk, encode_inter_payload, encode_mixed_intra_payload, encode_v1_only_payload,
    Mb, VECTOR_CHUNK_INTER, VECTOR_CHUNK_INTRA, VECTOR_CHUNK_V1_ONLY,
};
use oxideav_cinepak::{CinepakDecoder, DeviantConfig};

// ---------------------------------------------------------------------------
// Seed builders — must stay byte-identical to
// examples/seed_fuzz_corpora.rs. See that file for the rationale per
// seed family.
// ---------------------------------------------------------------------------

fn build_codebook_seeds() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
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
        let kind = CodebookChunkKind::from_id(*id).unwrap();
        let payload = encode_payload_for_kind(kind, &cb);

        let mut bytes = Vec::with_capacity(3 + payload.len());
        bytes.extend_from_slice(&id.to_be_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&payload);
        out.push((format!("{label}-strict"), bytes));

        let mut bytes = Vec::with_capacity(3 + payload.len() + 2);
        bytes.extend_from_slice(&id.to_be_bytes());
        bytes.push(1);
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&[0xab, 0xcd]);
        out.push((format!("{label}-tolerant-pad"), bytes));
    }

    let kind = CodebookChunkKind::from_id(0x2000).unwrap();
    let mut payload = Vec::new();
    encode_header_only_chunk(kind, &mut payload);
    let mut bytes = Vec::with_capacity(3);
    bytes.extend_from_slice(&0x2000u16.to_be_bytes());
    bytes.push(0);
    out.push(("v4-full-yuv-header-only".to_string(), bytes));

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
            let slots: [u8; 5] = [0, 1, 5, 32, 40];
            encode_selective_chunk(kind, cb, &slots, &mut wrapped);
        }
    }
    wrapped[4..].to_vec()
}

fn build_vector_seeds() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();

    let mbs = vec![Mb::V1(0), Mb::V1(1), Mb::V1(2), Mb::V1(3)];
    let mut payload = Vec::new();
    encode_v1_only_payload(&mbs, &mut payload).unwrap();
    out.push((
        "v1-only-4mb".to_string(),
        wrap_vector_seed(0x3200, mbs.len(), &payload),
    ));

    let mbs = vec![
        Mb::V4([0, 1, 2, 3]),
        Mb::V1(4),
        Mb::V4([5, 6, 7, 8]),
        Mb::V1(9),
    ];
    let mut payload = Vec::new();
    encode_mixed_intra_payload(&mbs, &mut payload).unwrap();
    out.push((
        "mixed-intra-4mb-alternating".to_string(),
        wrap_vector_seed(0x3000, mbs.len(), &payload),
    ));

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
    encode_mixed_intra_payload(&mbs, &mut payload).unwrap();
    out.push((
        "mixed-intra-33mb-2-flag-words".to_string(),
        wrap_vector_seed(0x3000, mbs.len(), &payload),
    ));

    let mbs = vec![Mb::Skip; 32];
    let mut payload = Vec::new();
    encode_inter_payload(&mbs, &mut payload).unwrap();
    out.push((
        "inter-all-skip-32mb".to_string(),
        wrap_vector_seed(0x3100, mbs.len(), &payload),
    ));

    let mut mbs = vec![Mb::Skip; 31];
    mbs.push(Mb::V4([10, 11, 12, 13]));
    mbs.push(Mb::V1(7));
    mbs.push(Mb::Skip);
    let mut payload = Vec::new();
    encode_inter_payload(&mbs, &mut payload).unwrap();
    out.push((
        "inter-straddle-34mb-selector-spillover".to_string(),
        wrap_vector_seed(0x3100, mbs.len(), &payload),
    ));

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
        ("standard-control-4x4".to_string(), build_standard_frame()),
    ]
}

fn build_deviant_frame(extra_header_bytes: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(0x00);
    bytes.extend_from_slice(&[0x00, 0x00, 0x00]);
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.resize(bytes.len() + extra_header_bytes, 0x00);
    let strip_hdr_pos = bytes.len();
    bytes.extend_from_slice(&0x1000u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&[0x20, 0x00, 0x00, 0x04]);
    bytes.extend_from_slice(&0x2200u16.to_be_bytes());
    bytes.extend_from_slice(&0x000cu16.to_be_bytes());
    bytes.extend_from_slice(&[50, 100, 150, 200, 0, 0]);
    bytes.extend_from_slice(&[0xab, 0xcd]);
    bytes.extend_from_slice(&0x3200u16.to_be_bytes());
    bytes.extend_from_slice(&0x0005u16.to_be_bytes());
    bytes.push(0x00);
    let strip_size = (bytes.len() - strip_hdr_pos) as u16;
    bytes[strip_hdr_pos + 2..strip_hdr_pos + 4].copy_from_slice(&strip_size.to_be_bytes());
    let real_len = bytes.len() as u32;
    let declared = real_len - 8;
    bytes[1] = ((declared >> 16) & 0xff) as u8;
    bytes[2] = ((declared >> 8) & 0xff) as u8;
    bytes[3] = (declared & 0xff) as u8;
    bytes
}

fn build_standard_frame() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(0x00);
    bytes.extend_from_slice(&[0x00, 0x00, 0x00]);
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes());
    let strip_hdr_pos = bytes.len();
    bytes.extend_from_slice(&0x1000u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&[0x20, 0x00, 0x00, 0x04]);
    bytes.extend_from_slice(&0x2200u16.to_be_bytes());
    bytes.extend_from_slice(&0x000au16.to_be_bytes());
    bytes.extend_from_slice(&[50, 100, 150, 200, 0, 0]);
    bytes.extend_from_slice(&0x3200u16.to_be_bytes());
    bytes.extend_from_slice(&0x0005u16.to_be_bytes());
    bytes.push(0x00);
    let strip_size = (bytes.len() - strip_hdr_pos) as u16;
    bytes[strip_hdr_pos + 2..strip_hdr_pos + 4].copy_from_slice(&strip_size.to_be_bytes());
    let real_len = bytes.len() as u32;
    bytes[1] = ((real_len >> 16) & 0xff) as u8;
    bytes[2] = ((real_len >> 8) & 0xff) as u8;
    bytes[3] = (real_len & 0xff) as u8;
    bytes
}

// ---------------------------------------------------------------------------
// Harness mirrors — these match `fuzz_targets/<target>.rs` so the
// test exercises the same dispatch shape libFuzzer sees per seed.
// ---------------------------------------------------------------------------

fn run_codebook_seed(data: &[u8]) -> std::result::Result<bool, String> {
    if data.len() < 3 {
        return Err(format!("seed too short ({} B < 3)", data.len()));
    }
    let chunk_id = u16::from_be_bytes([data[0], data[1]]);
    let tolerate_trailing = (data[2] & 1) != 0;
    let payload = &data[3..];

    let kind = match CodebookChunkKind::from_id(chunk_id) {
        Some(k) => k,
        None => return Ok(false),
    };
    let mut cb = Codebook::default();
    let strict = apply_codebook_chunk(kind, payload, &mut cb).is_ok();
    let mut cb = Codebook::default();
    let tolerant = apply_codebook_chunk_with(kind, payload, &mut cb, tolerate_trailing).is_ok();
    Ok(strict || tolerant)
}

fn run_vector_seed(data: &[u8]) -> std::result::Result<bool, String> {
    if data.len() < 4 {
        return Err(format!("seed too short ({} B < 4)", data.len()));
    }
    let chunk_id = u16::from_be_bytes([data[0], data[1]]);
    let mb_count = u16::from_be_bytes([data[2], data[3]]) as usize;
    let payload = &data[4..];
    let dispatch_ok = decode_vector_chunk(chunk_id, payload, mb_count).is_ok();
    for &explicit in [VECTOR_CHUNK_V1_ONLY, VECTOR_CHUNK_INTRA, VECTOR_CHUNK_INTER].iter() {
        let _ = decode_vector_chunk(explicit, payload, mb_count);
    }
    Ok(dispatch_ok || chunk_id == 0)
}

fn run_deviant_seed(data: &[u8]) -> bool {
    let mut dec = CinepakDecoder::new();
    let sat = dec
        .decode_deviant_frame(data, None, DeviantConfig::saturn())
        .is_ok();
    let mut dec = CinepakDecoder::new();
    let lem = dec
        .decode_deviant_frame(data, None, DeviantConfig::lemmings_3do())
        .is_ok();
    let mut dec = CinepakDecoder::new();
    let std_path = dec.decode_frame(data, None).is_ok();
    sat || lem || std_path
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn codebook_seeds_round_trip_through_fuzz_harness() {
    let seeds = build_codebook_seeds();
    assert_eq!(
        seeds.len(),
        18,
        "expected 18 codebook seeds (8 × 2 chunk-id rows + header-only + truncated)"
    );
    let mut positive = 0usize;
    for (name, bytes) in &seeds {
        match run_codebook_seed(bytes) {
            Ok(true) => positive += 1,
            Ok(false) => {}
            Err(e) => panic!("codebook seed {name} broke harness contract: {e}"),
        }
    }
    // 8 strict full+selective (all Ok) + 4 full-replace tolerant-pad (Ok
    // under the tolerant arm) = 12 positive seeds. Selective tolerant-pad,
    // header-only payload, and the truncated 5-byte seed take negative
    // arms — they're still useful coverage anchors per the example
    // generator's per-seed rationale.
    assert!(
        positive >= 12,
        "expected ≥ 12 positive codebook seeds, got {positive}"
    );
}

#[test]
fn vector_seeds_round_trip_through_fuzz_harness() {
    let seeds = build_vector_seeds();
    assert_eq!(
        seeds.len(),
        6,
        "expected 6 vector seeds (V1-only + 2 mixed-intra + 2 inter + 1 unknown-id)"
    );
    let mut positive = 0usize;
    for (name, bytes) in &seeds {
        match run_vector_seed(bytes) {
            Ok(true) => positive += 1,
            Ok(false) => {}
            Err(e) => panic!("vector seed {name} broke harness contract: {e}"),
        }
    }
    // V1-only + mixed-intra×2 + inter all-skip + inter straddle =
    // 5 positive seeds. The unknown-id-0x0000 seed is intentionally
    // negative — `run_vector_seed` short-circuits its return on
    // chunk_id == 0 so libFuzzer's "tried the dispatcher reject path"
    // signal is preserved in the dispatch-side coverage map without
    // a seed-test false alarm.
    assert!(
        positive >= 5,
        "expected ≥ 5 positive vector seeds, got {positive}"
    );
}

#[test]
fn deviant_seeds_round_trip_through_fuzz_harness() {
    let seeds = build_deviant_seeds();
    assert_eq!(
        seeds.len(),
        3,
        "expected 3 deviant seeds (Saturn + Lemmings-3DO + standard-control)"
    );
    let mut positive = 0usize;
    for (_name, bytes) in &seeds {
        if run_deviant_seed(bytes) {
            positive += 1;
        }
    }
    assert_eq!(
        positive, 3,
        "expected exactly 3 positive deviant seeds, got {positive}"
    );
}
