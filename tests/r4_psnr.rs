//! Round 4 PSNR_Y improvement validation.
//!
//! Round 3 (round-47) hit ~parity with ffmpeg's reference encoder
//! (~36.9 dB Y on the 64×64 gradient fixture). Round 4 adds **Lever E
//! — Linde-Buzo-Gray (LBG) split refinement** (`EncoderOptions::
//! lbg_max_passes`, default `8`): after the median-cut + Lloyd
//! warm-build, iteratively split the highest-distortion codebook slot
//! into the lowest-population slot and re-run Lloyd until no further
//! split improves total SSE.
//!
//! Reference: Linde, Buzo, Gray (1980) "An Algorithm for Vector
//! Quantizer Design", IEEE Trans. Communications 28(1) — published
//! VQ-design math, no proprietary source consulted.
//!
//! ## Round 3 vs Round 4 measured headline numbers
//!
//! | fixture                | r3 (lbg=0) | r4 (lbg=8) | delta |
//! | ---------------------- | ---------- | ---------- | ----- |
//! | 64×64 gradient encode_rgb24      | 36.69 dB | 37.85 dB | +1.16 dB |
//! | 64×64 gradient best_strips       | 40.23 dB | 40.77 dB | +0.53 dB |
//! | 320×240 gradient encode_rgb24    | 35.61 dB | 37.81 dB | +2.19 dB |
//! | 320×240 gradient best_strips     | 38.17 dB | 39.94 dB | +1.77 dB |
//! | 64×64 LCG-noise encode_rgb24     | 21.54 dB | 22.39 dB | +0.85 dB |
//! | 64×64 LCG-noise best_strips      | 22.09 dB | 22.98 dB | +0.89 dB |
//!
//! All measurements are self-encode + self-decode, BT.601 Y-channel
//! mean PSNR, at `EncoderOptions::from_quality(50)`.
//!
//! The test is *self-decode* (no ffmpeg dependency). PSNR_Y is reported
//! via `eprintln!` for round-4 changelog visibility.

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{encode_rgb24, encode_rgb24_best_strips, CinepakDecoder, EncoderOptions};

/// Synthesise a 320×240 smooth RGB24 gradient (matches `r3_psnr.rs`).
fn synth_320x240() -> Vec<u8> {
    let w: usize = 320;
    let h: usize = 240;
    let mut rgb = vec![0u8; w * h * 3];
    for r in 0..h {
        for c in 0..w {
            let off = r * w * 3 + c * 3;
            let red = ((c * 255) / (w - 1)) as u8;
            let blue = ((r * 255) / (h - 1)) as u8;
            let green = (((r + c) * 255) / (w + h - 2)) as u8;
            rgb[off] = red;
            rgb[off + 1] = green;
            rgb[off + 2] = blue;
        }
    }
    rgb
}

/// Synthesise a 64×64 smooth RGB24 gradient (matches `r3_psnr.rs`).
fn synth_64x64() -> Vec<u8> {
    let w: usize = 64;
    let h: usize = 64;
    let mut rgb = vec![0u8; w * h * 3];
    for r in 0..h {
        for c in 0..w {
            let off = r * w * 3 + c * 3;
            let red = ((c * 255) / (w - 1)) as u8;
            let blue = ((r * 255) / (h - 1)) as u8;
            let green = (((r + c) * 255) / (w + h - 2)) as u8;
            rgb[off] = red;
            rgb[off + 1] = green;
            rgb[off + 2] = blue;
        }
    }
    rgb
}

/// Synthesise a 64×64 deterministic random RGB24 fixture (LCG-based).
/// Each pixel byte is an independent draw from a Lehmer-style LCG seeded
/// at `0xDEADBEEF`; the resulting frame has zero spatial coherence,
/// which is the hardest content for a 4×4-block VQ codec like Cinepak.
fn synth_noisy_64x64() -> Vec<u8> {
    let w: usize = 64;
    let h: usize = 64;
    let mut rgb = vec![0u8; w * h * 3];
    let mut s: u32 = 0xDEAD_BEEF;
    for px in rgb.iter_mut() {
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        *px = (s >> 16) as u8;
    }
    rgb
}

/// BT.601 Y-channel mean PSNR between two RGB24 buffers.
fn psnr_y(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    assert!(a.len() % 3 == 0);
    let n = a.len() / 3;
    let mut sum_sq = 0.0f64;
    for i in 0..n {
        let ya =
            0.299 * a[i * 3] as f64 + 0.587 * a[i * 3 + 1] as f64 + 0.114 * a[i * 3 + 2] as f64;
        let yb =
            0.299 * b[i * 3] as f64 + 0.587 * b[i * 3 + 1] as f64 + 0.114 * b[i * 3 + 2] as f64;
        let d = ya - yb;
        sum_sq += d * d;
    }
    if sum_sq == 0.0 {
        return f64::INFINITY;
    }
    let mse = sum_sq / n as f64;
    20.0 * (255.0_f64).log10() - 10.0 * mse.log10()
}

fn decode_packed(bytes: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(bytes, None).unwrap();
    let stride = f.stride();
    let mut packed = Vec::with_capacity((w * h * 3) as usize);
    for r in 0..h as usize {
        let off = r * stride;
        packed.extend_from_slice(&f.pixels()[off..off + (w as usize) * 3]);
    }
    packed
}

fn encode_decode_measure(rgb: &[u8], w: u32, h: u32, opts: EncoderOptions) -> (Vec<u8>, f64) {
    let bytes = encode_rgb24(rgb, w, h, opts).unwrap();
    let psnr = psnr_y(rgb, &decode_packed(&bytes, w, h));
    (bytes, psnr)
}

/// Round-4 baseline isolation: pin `luma_weight = 1` so the
/// round-5 luma-weighted distance lever doesn't mix with the
/// `lbg_max_passes` baseline these tests characterise. Also pins
/// `pcl_max_iter = 0` (round-6 Lever H), since the post-classification
/// Lloyd polish does its own re-training of the LBG output and would
/// otherwise eat most of the LBG-only delta this test isolates. Pins
/// `kmeans_pp_init = false` (round-9 Lever M) for the same reason:
/// k-means++ initialisation gives the cold-start codebook a much
/// stronger starting point, which compresses the LBG-only delta. These
/// tests measure the **round-4** LBG lever's contribution, so we pin
/// the cold-start to the original median-cut baseline.
fn r4_isolated_opts(quality: u8) -> EncoderOptions {
    EncoderOptions {
        luma_weight: 1,
        pcl_max_iter: 0,
        kmeans_pp_init: false,
        ..EncoderOptions::from_quality(quality)
    }
}

/// **Lever E target — 64×64 gradient ≥ 38 dB PSNR_Y.**
///
/// Uses the per-frame strip-count picker (Lever A, round 3) on top of
/// the new round-4 LBG-refined codebook. The picker selects 4 strips on
/// this fixture (each strip's 64×16 region trains a codebook against a
/// narrower luma/chroma range), and the LBG split refinement extracts
/// the residual codebook-coverage error that median-cut + 2-iter Lloyd
/// alone left on the table. Combined PSNR_Y observed: ~40.77 dB at
/// 2689 B — well past the 38 dB target and well past ffmpeg's reference
/// 36.9 dB on this fixture.
#[test]
fn lever_e_64x64_gradient_breaks_38db() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions::from_quality(50);
    assert_eq!(
        opts.lbg_max_passes, 8,
        "LBG default should be 8 passes (round-4 lever E)"
    );

    let bytes = encode_rgb24_best_strips(&rgb, w, h, opts, &[1, 2, 4]).unwrap();
    let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
    eprintln!(
        "Lever E (LBG) on 64x64 gradient via strip-picker at q=50: \
         psnr_y = {:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 38.0,
        "Lever-E target: PSNR_Y on 64x64 gradient should be >= 38 dB; got {psnr:.3}"
    );
}

/// **No regression at 320×240** — the round-3 strip-picker baseline was
/// 38.17 dB on this fixture; with round-4 LBG layered on top we expect
/// ~39.94 dB. Assert >= 38 dB so a minor LBG-tuning regression on a
/// non-target fixture wouldn't silently land.
#[test]
fn lever_e_320x240_gradient_no_regression() {
    let rgb = synth_320x240();
    let w = 320u32;
    let h = 240u32;
    let opts = EncoderOptions::from_quality(50);

    let bytes = encode_rgb24_best_strips(&rgb, w, h, opts, &[1, 2, 4]).unwrap();
    let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
    eprintln!(
        "Lever E (LBG) on 320x240 gradient via strip-picker at q=50: \
         psnr_y = {:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 38.0,
        "320x240 gradient must stay >= 38 dB after round-4; got {psnr:.3}"
    );
}

/// **Lever E benefit on textured/noisy content.** Pure-noise content
/// has zero spatial coherence — the worst case for a 4×4 VQ codec. Even
/// here LBG refinement extracts a measurable improvement vs the
/// round-3 `lbg_max_passes = 0` baseline. Assert delta >= 0.5 dB.
#[test]
fn lever_e_noisy_64x64_improves_by_half_db() {
    let rgb = synth_noisy_64x64();
    let w = 64u32;
    let h = 64u32;
    // Pin `luma_weight = 1` so the round-5 luma-weighted distance lever
    // doesn't compound with the r4 LBG delta being measured here.
    let base = r4_isolated_opts(50);

    let (bytes_r3, psnr_r3) = encode_decode_measure(
        &rgb,
        w,
        h,
        EncoderOptions {
            lbg_max_passes: 0,
            ..base
        },
    );
    let (bytes_r4, psnr_r4) = encode_decode_measure(
        &rgb,
        w,
        h,
        EncoderOptions {
            lbg_max_passes: 8,
            ..base
        },
    );
    let delta = psnr_r4 - psnr_r3;
    eprintln!(
        "Lever E (LBG) on 64x64 LCG-noise at q=50: \
         r3 (lbg=0) psnr_y={:.3} dB ({} B), \
         r4 (lbg=8) psnr_y={:.3} dB ({} B), \
         delta = +{:.3} dB",
        psnr_r3,
        bytes_r3.len(),
        psnr_r4,
        bytes_r4.len(),
        delta
    );
    assert!(
        delta >= 0.5,
        "Lever-E LBG on noisy fixture should lift PSNR_Y by >=0.5 dB; got {delta:+.3}"
    );
}

/// **Smooth-gradient delta sanity** — confirm the LBG default lifts
/// PSNR_Y by at least 1 dB on the 64×64 gradient that hit ffmpeg
/// reference parity in round 3. Direct comparison: `lbg_max_passes = 0`
/// (round-3 behaviour) vs `lbg_max_passes = 8` (round-4 default).
#[test]
fn lever_e_64x64_gradient_lbg_delta() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    // Pin `luma_weight = 1` so the round-5 luma-weighted distance lever
    // doesn't compound with the r4 LBG delta being measured here.
    let base = r4_isolated_opts(50);

    let (bytes_r3, psnr_r3) = encode_decode_measure(
        &rgb,
        w,
        h,
        EncoderOptions {
            lbg_max_passes: 0,
            ..base
        },
    );
    let (bytes_r4, psnr_r4) = encode_decode_measure(
        &rgb,
        w,
        h,
        EncoderOptions {
            lbg_max_passes: 8,
            ..base
        },
    );
    let delta = psnr_r4 - psnr_r3;
    eprintln!(
        "Lever E (LBG) on 64x64 gradient at q=50: \
         r3 (lbg=0) psnr_y={:.3} dB ({} B), \
         r4 (lbg=8) psnr_y={:.3} dB ({} B), \
         delta = +{:.3} dB",
        psnr_r3,
        bytes_r3.len(),
        psnr_r4,
        bytes_r4.len(),
        delta
    );
    assert!(
        delta >= 1.0,
        "Lever-E LBG on 64x64 gradient should lift PSNR_Y by >=1 dB; got {delta:+.3}"
    );
}

/// **`lbg_max_passes = 0` regression guard** — disabling LBG must
/// restore the round-3 baseline (RDO+strip-picker only). PSNR_Y on
/// 64×64 gradient should remain at the round-3 reference of ~36.69 dB
/// (assert ≥ 36.5 dB to allow noise).
#[test]
fn lbg_disabled_matches_round3_baseline() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    // Pin `luma_weight = 1` so the round-5 luma-weighted distance lever
    // doesn't compound with the r4 LBG baseline being characterised here.
    let opts = EncoderOptions {
        lbg_max_passes: 0,
        ..r4_isolated_opts(50)
    };
    let (bytes, psnr) = encode_decode_measure(&rgb, w, h, opts);
    eprintln!(
        "lbg=0 (r3 baseline, luma_weight=1) on 64x64 gradient at q=50: \
         psnr_y={:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 36.5,
        "lbg=0 should reproduce r3 baseline (>= 36.5 dB); got {psnr:.3}"
    );
    // And LBG-enabled (at the same luma_weight=1 pin) must strictly
    // improve on it.
    let opts = r4_isolated_opts(50);
    let (_, psnr_lbg) = encode_decode_measure(&rgb, w, h, opts);
    assert!(
        psnr_lbg > psnr,
        "LBG should strictly improve over lbg=0 baseline: lbg=0 {psnr:.3} vs default {psnr_lbg:.3}"
    );
}
