//! Round 383 milestone 6 — encoder self-conformance property test.
//!
//! Every public encode entry point must emit a stream the crate's own
//! wire-format linter (`lint_frame` / `lint_frame_with`) finds
//! completely clean: zero errors *and* zero warnings. The lint rules
//! are grounded in `docs/video/cinepak/spec/01-frame-and-strip.md`,
//! `02-codebooks.md`, `03-vectors-and-macroblocks.md`, and
//! `04-yuv-rgb-matrix.md`; an encoder regression that starts emitting
//! e.g. mis-accounted chunk sizes, out-of-order codebook chunks, or
//! out-of-range vector indices trips this test rather than waiting
//! for a downstream decoder to notice.

use oxideav_cinepak::encoder::{
    encode_gray8, encode_gray8_best_rd_grid, encode_gray8_inter, encode_gray8_round7, encode_rgb24,
    encode_rgb24_best_rd_grid, encode_rgb24_best_rd_grid_3axis, encode_rgb24_best_strips,
    encode_rgb24_inter, encode_rgb24_per_strip_rd, encode_rgb24_round6, encode_rgb24_round7,
    encode_rgb24_round8, CinepakEncoder, EncoderOptions,
};
use oxideav_cinepak::{lint_frame, lint_frame_with, lint_sequence, CinepakDecoder, LintOptions};

const W: u32 = 32;
const H: u32 = 16;

/// Deterministic mid-detail RGB frame (forces both V1 and V4 blocks).
fn rgb_frame(seed: u32) -> Vec<u8> {
    (0..(W * H * 3))
        .map(|i| ((i * 31 + seed * 17) % 251) as u8)
        .collect()
}

fn gray_frame(seed: u32) -> Vec<u8> {
    (0..(W * H))
        .map(|i| ((i * 13 + seed * 7) % 256) as u8)
        .collect()
}

fn assert_clean(label: &str, bytes: &[u8]) {
    let rep = lint_frame(bytes);
    assert!(
        rep.is_clean(),
        "{label}: encoder output has lint findings: {:?}",
        rep.issues()
    );
}

#[test]
fn intra_entry_points_lint_clean() {
    let rgb = rgb_frame(0);
    let opts = EncoderOptions::default();
    assert_clean("encode_rgb24", &encode_rgb24(&rgb, W, H, opts).unwrap());
    assert_clean(
        "encode_rgb24_best_strips",
        &encode_rgb24_best_strips(&rgb, W, H, opts, &[1, 2, 4]).unwrap(),
    );
    assert_clean(
        "encode_rgb24_best_rd_grid",
        &encode_rgb24_best_rd_grid(&rgb, W, H, opts, &[1, 2], &[None, Some(0.5)]).unwrap(),
    );
    assert_clean(
        "encode_rgb24_round6",
        &encode_rgb24_round6(&rgb, W, H, opts).unwrap(),
    );
    assert_clean(
        "encode_rgb24_best_rd_grid_3axis",
        &encode_rgb24_best_rd_grid_3axis(&rgb, W, H, opts, &[1, 2], &[None], &[1, 2]).unwrap(),
    );
    assert_clean(
        "encode_rgb24_round7",
        &encode_rgb24_round7(&rgb, W, H, opts).unwrap(),
    );
    assert_clean(
        "encode_rgb24_per_strip_rd",
        &encode_rgb24_per_strip_rd(&rgb, W, H, opts, &[2], &[None, Some(1.0)], &[1]).unwrap(),
    );
    assert_clean(
        "encode_rgb24_round8",
        &encode_rgb24_round8(&rgb, W, H, opts).unwrap(),
    );
}

#[test]
fn gray_entry_points_lint_clean() {
    let gray = gray_frame(0);
    let opts = EncoderOptions::default();
    assert_clean("encode_gray8", &encode_gray8(&gray, W, H, opts).unwrap());
    assert_clean(
        "encode_gray8_best_rd_grid",
        &encode_gray8_best_rd_grid(&gray, W, H, opts, &[1, 2], &[None, Some(0.5)]).unwrap(),
    );
    assert_clean(
        "encode_gray8_round7",
        &encode_gray8_round7(&gray, W, H, opts).unwrap(),
    );
}

#[test]
fn stateless_inter_entry_points_lint_clean() {
    let opts = EncoderOptions::default();

    let f0 = rgb_frame(0);
    let mut f1 = rgb_frame(0);
    for px in f1.iter_mut().take(60) {
        *px = px.wrapping_add(80);
    }
    let intra = encode_rgb24(&f0, W, H, opts).unwrap();
    let mut dec = CinepakDecoder::new();
    let prev = dec.decode_frame(&intra, None).unwrap();
    assert_clean(
        "encode_rgb24_inter",
        &encode_rgb24_inter(&f1, &prev, W, H, opts).unwrap(),
    );

    let g0 = gray_frame(0);
    let mut g1 = gray_frame(0);
    for px in g1.iter_mut().take(40) {
        *px = px.wrapping_add(70);
    }
    let gintra = encode_gray8(&g0, W, H, opts).unwrap();
    let mut gdec = CinepakDecoder::new();
    let gprev = gdec.decode_frame(&gintra, None).unwrap();
    assert_clean(
        "encode_gray8_inter",
        &encode_gray8_inter(&g1, &gprev, W, H, opts).unwrap(),
    );
}

#[test]
fn stateful_encoder_multi_strip_gop_lints_clean_as_sequence() {
    let opts = EncoderOptions {
        strip_count: 2,
        ..EncoderOptions::default()
    };
    let mut enc = CinepakEncoder::new().with_keyframe_interval(3);
    let mut frames = Vec::new();
    for n in 0..6u32 {
        let px = rgb_frame(n);
        frames.push(enc.encode_frame(&px, W, H, opts).unwrap().bytes);
    }
    let reps = lint_sequence(frames.iter().map(|f| f.as_slice()), &LintOptions::new());
    for (n, rep) in reps.iter().enumerate() {
        assert!(
            rep.is_clean(),
            "GOP frame {n}: lint findings: {:?}",
            rep.issues()
        );
    }
}

#[test]
fn stateful_gray_encoder_lints_clean() {
    let opts = EncoderOptions::default();
    let mut enc = CinepakEncoder::new();
    let g0 = gray_frame(1);
    let g1 = gray_frame(2);
    assert_clean(
        "encode_intra_gray8",
        &enc.encode_intra_gray8(&g0, W, H, opts).unwrap(),
    );
    assert_clean(
        "encode_inter_gray8",
        &enc.encode_inter_gray8(&g1, W, H, opts).unwrap(),
    );
}

#[test]
fn rate_controlled_encoder_lints_clean() {
    let opts = EncoderOptions::default();
    let mut enc = CinepakEncoder::new().with_target_frame_bytes(600);
    let intra = enc.encode_intra(&rgb_frame(0), W, H, opts).unwrap();
    let inter = enc.encode_inter(&rgb_frame(1), W, H, opts).unwrap();
    assert_clean("budgeted intra", &intra);
    assert_clean("budgeted inter", &inter);
}

#[test]
fn vintage_compat_encoder_passes_vintage_profile_over_a_gop() {
    let opts = EncoderOptions {
        vintage_compat: true,
        strip_count: 3,
        ..EncoderOptions::default()
    };
    let vintage = LintOptions::new().with_vintage(true);
    let mut enc = CinepakEncoder::new().with_keyframe_interval(2);
    for n in 0..4u32 {
        // Alternate identical frames in so inter frames carry skips
        // and header-only reuse chunks — the shapes the vintage rules
        // scrutinise.
        let px = rgb_frame(n / 2);
        let out = enc.encode_frame(&px, W, H, opts).unwrap();
        let rep = lint_frame_with(&out.bytes, &vintage);
        assert!(
            rep.is_clean(),
            "vintage GOP frame {n} (keyframe: {}): {:?}",
            out.is_keyframe,
            rep.issues()
        );
    }
}

#[test]
fn quality_knob_sweep_lints_clean() {
    for q in [0u8, 25, 50, 75, 100] {
        let opts = EncoderOptions::from_quality(q);
        let bytes = encode_rgb24(&rgb_frame(q as u32), W, H, opts).unwrap();
        let rep = lint_frame(&bytes);
        assert!(rep.is_clean(), "q = {q}: {:?}", rep.issues());
    }
}
