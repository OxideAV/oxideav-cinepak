//! Round 5 PSNR_Y improvement validation.
//!
//! Round 4 (`b90ec7f`) landed Linde-Buzo-Gray (LBG) split refinement
//! and hit 40.77 dB PSNR_Y on the 64×64 gradient via the strip-picker.
//! Round 5 adds **two complementary luma-priority levers** at the
//! codebook-training layer:
//!
//! - **Lever F — luma-weighted distance metric**
//!   (`EncoderOptions::luma_weight`, default `2`): each Y-dim squared-
//!   error contribution is multiplied by `luma_weight` before being
//!   summed with the chroma U/V contributions. This affects
//!   nearest-neighbour selection (per-MB classification), Lloyd
//!   refinement, and LBG split refinement. Under PSNR_Y, packing the
//!   codebook tightly in Y is more valuable than packing it tightly in
//!   U/V — luma-weighted clustering pulls trained centroids closer to
//!   the source Y values at a modest fidelity cost on chroma.
//!
//! - **Lever G — luma-prioritized median-cut split**: the same
//!   `luma_weight` multiplies Y-dim extents when picking the split
//!   dimension in `median_cut`, biasing the initial bisection toward
//!   Y-axis cuts when Y and U/V extents are comparable.
//!
//! ## Round 4 vs Round 5 measured headline numbers
//!
//! Self-encode + self-decode at `EncoderOptions::from_quality(50)`,
//! BT.601 Y-channel mean PSNR.
//!
//! | fixture                                        | r4 (lw=1) | r5 (lw=2) | delta     |
//! | ---------------------------------------------- | --------- | --------- | --------- |
//! | 64×64 gradient `encode_rgb24`                  | 37.85 dB  | 39.39 dB  | +1.55 dB  |
//! | 64×64 gradient `encode_rgb24_best_strips`      | 40.77 dB  | 42.39 dB  | +1.62 dB  |
//! | 320×240 gradient `encode_rgb24_best_strips`    | 40.69 dB  | 41.70 dB  | +1.01 dB  |
//! | 64×64 LCG-noise `encode_rgb24_best_strips`     | 22.98 dB  | 22.98 dB  | ±0.0  dB  |
//!
//! Lever F+G compound: the round-5 default (`luma_weight = 2`) lifts
//! the 64×64 gradient via the strip-picker past the round-5 41.27 dB
//! target with significant headroom. On LCG-noise the lever is a
//! near-wash (within ±0.05 dB) — pure noise has no luma-vs-chroma
//! structure to exploit; the lever neither helps nor hurts.

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{encode_rgb24, encode_rgb24_best_strips, CinepakDecoder, EncoderOptions};

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

/// **Lever F+G target — 64×64 gradient ≥ 41.27 dB PSNR_Y via the
/// strip-picker.**
///
/// Combines the round-4 LBG split-refinement codebook with the
/// round-5 luma-weighted distance + luma-prioritized median-cut
/// split, then drives `encode_rgb24_best_strips` over strip counts
/// `[1, 2, 4]` (default picker grid). The Lever F+G default lifts the
/// fixture from r4's 40.77 dB to ~42.39 dB Y at 2554 B.
#[test]
fn lever_f_g_64x64_gradient_breaks_41_27db() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions::from_quality(50);
    assert_eq!(
        opts.luma_weight, 2,
        "round-5 default luma_weight should be 2 (Lever F)"
    );

    let bytes = encode_rgb24_best_strips(&rgb, w, h, opts, &[1, 2, 4]).unwrap();
    let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
    eprintln!(
        "Lever F+G on 64x64 gradient via strip-picker at q=50: \
         psnr_y = {:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 41.27,
        "Lever-F+G target: PSNR_Y on 64x64 gradient should be >= 41.27 dB; got {psnr:.3}"
    );
}

/// **Lever F+G target — 320×240 gradient ≥ 41.0 dB PSNR_Y via the
/// strip-picker.** Round 4 hit 40.69 dB on this fixture; the round-5
/// luma-priority levers lift it past the 41.0 dB target with headroom
/// (observed ~41.70 dB at 9288 B).
#[test]
fn lever_f_g_320x240_gradient_breaks_41db() {
    let rgb = synth_320x240();
    let w = 320u32;
    let h = 240u32;
    let opts = EncoderOptions::from_quality(50);

    let bytes = encode_rgb24_best_strips(&rgb, w, h, opts, &[1, 2, 4]).unwrap();
    let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
    eprintln!(
        "Lever F+G on 320x240 gradient via strip-picker at q=50: \
         psnr_y = {:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 41.0,
        "Lever-F+G target: PSNR_Y on 320x240 gradient should be >= 41.0 dB; got {psnr:.3}"
    );
}

/// **LCG-noise no-regression** — pure noise has no luma-vs-chroma
/// structure to exploit; the round-5 levers should be a near-wash on
/// this fixture (no improvement, no regression). Asserts the round-5
/// strip-picker output stays within −0.5 dB of round-4's 22.98 dB.
#[test]
fn lever_f_g_noisy_64x64_no_regression() {
    let rgb = synth_noisy_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions::from_quality(50);

    let bytes = encode_rgb24_best_strips(&rgb, w, h, opts, &[1, 2, 4]).unwrap();
    let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
    eprintln!(
        "Lever F+G on 64x64 LCG-noise via strip-picker at q=50: \
         psnr_y = {:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    // Round-4 hit 22.98 dB on this fixture; round-5 should stay within
    // 0.5 dB of that floor (pure-noise content has no luma-vs-chroma
    // structure to exploit, so the lever neither helps nor meaningfully
    // hurts).
    assert!(
        psnr >= 22.48,
        "Lever-F+G should not regress on LCG-noise: PSNR_Y >= 22.48 dB; got {psnr:.3}"
    );
}

/// **Lever F (luma-weight) isolated delta — 64×64 gradient strict
/// improvement.** Direct A/B between `luma_weight = 1` (round-4
/// isotropic baseline) and `luma_weight = 2` (round-5 default).
/// Assert the round-5 default lifts PSNR_Y by at least 1.0 dB on the
/// 64×64 gradient at q=50 in the raw (single-strip) path. Observed
/// delta: ~+1.55 dB (37.85 → 39.39 dB).
#[test]
fn lever_f_luma_weight_64x64_gradient_lifts_by_1db() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let base = EncoderOptions::from_quality(50);

    let (bytes_r4, psnr_r4) = encode_decode_measure(
        &rgb,
        w,
        h,
        EncoderOptions {
            luma_weight: 1,
            ..base
        },
    );
    let (bytes_r5, psnr_r5) = encode_decode_measure(
        &rgb,
        w,
        h,
        EncoderOptions {
            luma_weight: 2,
            ..base
        },
    );
    let delta = psnr_r5 - psnr_r4;
    eprintln!(
        "Lever F (luma_weight) on 64x64 gradient at q=50: \
         r4 (lw=1) psnr_y={:.3} dB ({} B), \
         r5 (lw=2) psnr_y={:.3} dB ({} B), \
         delta = +{:.3} dB",
        psnr_r4,
        bytes_r4.len(),
        psnr_r5,
        bytes_r5.len(),
        delta
    );
    assert!(
        delta >= 1.0,
        "Lever-F luma_weight=2 should lift PSNR_Y by >= 1 dB on 64x64 gradient; got {delta:+.3}"
    );
}

/// **`luma_weight = 1` regression guard** — pinning the round-5 lever
/// off must restore the round-4 isotropic-distance behaviour on the
/// 64×64 gradient (~37.85 dB PSNR_Y at q=50). Assert ≥ 37.5 dB to
/// allow for measurement noise across builds.
#[test]
fn luma_weight_1_restores_round4_baseline() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions {
        luma_weight: 1,
        ..EncoderOptions::from_quality(50)
    };
    let (bytes, psnr) = encode_decode_measure(&rgb, w, h, opts);
    eprintln!(
        "luma_weight=1 (round-4 baseline) on 64x64 gradient at q=50: \
         psnr_y={:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 37.5,
        "luma_weight=1 should reproduce round-4 baseline (>= 37.5 dB); got {psnr:.3}"
    );
    // And luma_weight=2 (round-5 default) must strictly improve.
    let opts_r5 = EncoderOptions::from_quality(50);
    let (_, psnr_r5) = encode_decode_measure(&rgb, w, h, opts_r5);
    assert!(
        psnr_r5 > psnr,
        "luma_weight=2 should strictly improve over luma_weight=1: \
         lw=1 {psnr:.3} vs lw=2 {psnr_r5:.3}"
    );
}

/// **`luma_weight = 0` is treated as `1` (no-op fallback).** Sanity
/// check that the encoder accepts `luma_weight = 0` and produces a
/// stream equivalent to `luma_weight = 1` (same byte count + same
/// decoded PSNR_Y).
#[test]
fn luma_weight_0_falls_back_to_1() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let base = EncoderOptions::from_quality(50);

    let (bytes_0, psnr_0) = encode_decode_measure(
        &rgb,
        w,
        h,
        EncoderOptions {
            luma_weight: 0,
            ..base
        },
    );
    let (bytes_1, psnr_1) = encode_decode_measure(
        &rgb,
        w,
        h,
        EncoderOptions {
            luma_weight: 1,
            ..base
        },
    );
    eprintln!(
        "luma_weight=0 → fallback: psnr_y={:.3} dB ({} B); \
         luma_weight=1: psnr_y={:.3} dB ({} B)",
        psnr_0,
        bytes_0.len(),
        psnr_1,
        bytes_1.len(),
    );
    assert_eq!(
        bytes_0.len(),
        bytes_1.len(),
        "luma_weight=0 should be byte-equivalent to luma_weight=1"
    );
    assert!(
        (psnr_0 - psnr_1).abs() < 1e-6,
        "luma_weight=0 and luma_weight=1 should produce identical PSNR; got {psnr_0:.6} vs {psnr_1:.6}"
    );
}
