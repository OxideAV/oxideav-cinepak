//! `0x3100` inter-vector-chunk roundtrip regression tests covering
//! the V1/V4 selector-bit-spillover edge cases (round 5).
//!
//! These trigger flag-word boundaries in the encoder that were
//! previously not exercised by the round-1 / round-3 inter roundtrip
//! tests; they caught a decoder bug where the deferred
//! placeholder-mb's index bytes were being read in the wrong group.

use oxideav_cinepak::vector::{decode_vector_chunk, encode_inter_payload, Mb, VECTOR_CHUNK_INTER};

#[test]
fn inter_payload_roundtrip_dense_v4_64mb() {
    // 64 V4 macroblocks: each contributes `0b11` = 2 bits → 128 bits total
    // = exactly 4 flag words. Each V4 carries 4 index bytes → 256 idx bytes.
    let mbs: Vec<Mb> = (0..64)
        .map(|i| Mb::V4([i as u8, i as u8 + 1, i as u8 + 2, i as u8 + 3]))
        .collect();
    let mut buf = Vec::new();
    encode_inter_payload(&mbs, &mut buf).unwrap();
    eprintln!("dense-V4 64-mb payload: {} bytes", buf.len());
    let out = decode_vector_chunk(VECTOR_CHUNK_INTER, &buf, mbs.len()).unwrap();
    assert_eq!(out, mbs);
}

#[test]
fn inter_payload_roundtrip_dense_v1_64mb() {
    // 64 V1 macroblocks: each `0b10` = 2 bits → 128 bits = 4 flag words.
    // Each V1 carries 1 idx byte = 64 idx bytes.
    let mbs: Vec<Mb> = (0..64).map(|i| Mb::V1(i as u8)).collect();
    let mut buf = Vec::new();
    encode_inter_payload(&mbs, &mut buf).unwrap();
    eprintln!("dense-V1 64-mb payload: {} bytes", buf.len());
    let out = decode_vector_chunk(VECTOR_CHUNK_INTER, &buf, mbs.len()).unwrap();
    assert_eq!(out, mbs);
}

#[test]
fn inter_payload_roundtrip_skip_v1_v4_dense_64mb() {
    let mut mbs: Vec<Mb> = Vec::new();
    for i in 0..64 {
        mbs.push(match i % 4 {
            0 => Mb::Skip,
            1 => Mb::V1(i as u8),
            2 => Mb::V4([i as u8, 1, 2, 3]),
            3 => Mb::V1((i + 100) as u8),
            _ => unreachable!(),
        });
    }
    let mut buf = Vec::new();
    encode_inter_payload(&mbs, &mut buf).unwrap();
    eprintln!("mixed 64-mb payload: {} bytes", buf.len());
    let out = decode_vector_chunk(VECTOR_CHUNK_INTER, &buf, mbs.len()).unwrap();
    assert_eq!(out, mbs);
}

#[test]
fn inter_payload_roundtrip_dense_v4_16mb() {
    // 16 V4 macroblocks: each `0b11` = 2 bits → 32 bits = exactly 1 flag word.
    let mbs: Vec<Mb> = (0..16)
        .map(|i| Mb::V4([i as u8, i as u8 + 1, i as u8 + 2, i as u8 + 3]))
        .collect();
    let mut buf = Vec::new();
    encode_inter_payload(&mbs, &mut buf).unwrap();
    eprintln!("dense-V4 16-mb payload: {} bytes", buf.len());
    let out = decode_vector_chunk(VECTOR_CHUNK_INTER, &buf, mbs.len()).unwrap();
    assert_eq!(out, mbs);
}
