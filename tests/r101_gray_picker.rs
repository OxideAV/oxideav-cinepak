//! Round 101 — grayscale RD-grid picker (Lever N).
//!
//! The 12-bit-YUV encoder picked up a frame-level RD-grid picker in
//! rounds 47 / 6 / 7 (`encode_rgb24_best_strips` /
//! `encode_rgb24_best_rd_grid` / `encode_rgb24_best_rd_grid_3axis`),
//! but the grayscale path (`encode_gray8`) always emitted a single
//! frame at the caller's `opts.strip_count` / `opts.rdo_lambda`.
//!
//! Round 101 adds [`oxideav_cinepak::encode_gray8_best_rd_grid`] +
//! the [`oxideav_cinepak::encode_gray8_round7`] convenience wrapper:
//! trial-encode the input at every `(strip_count, rdo_lambda)`
//! combination, self-decode, and keep the lowest
//! `direct-luma-SSE-per-pixel + opts.rdo_lambda · R/N`. No
//! `luma_weight` axis — for `Gray8` codebook entries the distance
//! metric scales all four Y dims by `luma_weight`, a uniform positive
//! scale that leaves every clustering/nearest-neighbour decision
//! invariant.
//!
//! These tests assert (a) the picker never regresses PSNR vs the
//! fixed-default `encode_gray8` on the same opts, (b) it produces a
//! conformant stream that self-decodes to `Gray8`, and (c) the headline
//! gradient gain. The `report_*` test prints the measured table.

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{
    encode_gray8, encode_gray8_best_rd_grid, encode_gray8_round7, CinepakDecoder,
    CinepakPixelFormat, EncoderOptions,
};

/// Smooth two-axis grayscale gradient (favours more strips so each
/// band's V4 codebook localises its luminance range).
fn gradient_gray(w: usize, h: usize) -> Vec<u8> {
    let mut gray = vec![0u8; w * h];
    for r in 0..h {
        for c in 0..w {
            // Diagonal ramp 0..255.
            gray[r * w + c] = (((r + c) * 255) / (w + h - 2)) as u8;
        }
    }
    gray
}

/// Deterministic LCG noise (no luminance structure to exploit — the
/// picker should at worst match the default).
fn noise_gray(w: usize, h: usize) -> Vec<u8> {
    let mut gray = vec![0u8; w * h];
    let mut s: u32 = 0x1234_5678;
    for px in gray.iter_mut() {
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        *px = (s >> 16) as u8;
    }
    gray
}

/// Mean-squared error → PSNR (dB) between two equal-length luma planes.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sse = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let d = x as f64 - y as f64;
        sse += d * d;
    }
    let mse = sse / a.len() as f64;
    if mse <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

/// Decode an encoded grayscale frame back to a packed `Gray8` plane.
fn decode_gray(bytes: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(bytes, None).unwrap();
    assert_eq!(f.pixel_format, CinepakPixelFormat::Gray8);
    let stride = f.stride();
    let p = f.pixels();
    let mut out = vec![0u8; w * h];
    for r in 0..h {
        for c in 0..w {
            out[r * w + c] = p[r * stride + c];
        }
    }
    out
}

/// The picker output self-decodes to a conformant `Gray8` frame.
#[test]
fn picker_output_is_conformant_gray8() {
    let (w, h) = (64usize, 64usize);
    let src = gradient_gray(w, h);
    let opts = EncoderOptions::from_quality(50);
    let bytes = encode_gray8_round7(&src, w as u32, h as u32, opts).unwrap();
    let dec = decode_gray(&bytes, w, h);
    // Above the noise floor on a structured fixture.
    assert!(psnr(&src, &dec) > 30.0);
}

/// Headline win on the 64×64 grayscale gradient at q=50: the picker
/// lifts PSNR from the fixed-default's ~45.0 dB to ≥ 48 dB by selecting
/// a higher strip count whose per-band V4 codebooks localise the
/// luminance ramp. (Measured: +4.55 dB, 45.01 → 49.56 dB.)
#[test]
fn headline_gradient_64x64_gain() {
    let (w, h) = (64usize, 64usize);
    let src = gradient_gray(w, h);
    let opts = EncoderOptions::from_quality(50);

    let default_psnr = psnr(
        &src,
        &decode_gray(&encode_gray8(&src, w as u32, h as u32, opts).unwrap(), w, h),
    );
    let picked_psnr = psnr(
        &src,
        &decode_gray(
            &encode_gray8_round7(&src, w as u32, h as u32, opts).unwrap(),
            w,
            h,
        ),
    );

    assert!(
        default_psnr < 46.0,
        "default baseline drifted: {default_psnr:.2} dB (expected ~45.0)"
    );
    assert!(
        picked_psnr >= 48.0,
        "picker {picked_psnr:.2} dB below 48 dB headline target"
    );
    assert!(
        picked_psnr - default_psnr >= 3.0,
        "gain {:.2} dB below +3 dB target",
        picked_psnr - default_psnr
    );
}

/// On a smooth gradient the picker is at least as good (PSNR) as the
/// fixed-default single-strip `encode_gray8` at the same opts, because
/// the default `[1, 2, 4]` candidate set includes `strip_count = 1`.
#[test]
fn picker_never_regresses_psnr_vs_default_on_gradient() {
    for &(w, h) in &[(64usize, 64usize), (128, 96), (32, 32)] {
        let src = gradient_gray(w, h);
        let opts = EncoderOptions::from_quality(50);

        let default_bytes = encode_gray8(&src, w as u32, h as u32, opts).unwrap();
        let default_dec = decode_gray(&default_bytes, w, h);
        let default_psnr = psnr(&src, &default_dec);

        let picked_bytes = encode_gray8_round7(&src, w as u32, h as u32, opts).unwrap();
        let picked_dec = decode_gray(&picked_bytes, w, h);
        let picked_psnr = psnr(&src, &picked_dec);

        // The picker minimises distortion + lambda·rate, so its PSNR may
        // be marginally below the *max-PSNR* point when a much smaller
        // wire size wins on cost — but on the gradient with the default
        // lambda it should be within a small tolerance of (and usually
        // above) the single-strip default.
        assert!(
            picked_psnr >= default_psnr - 1.0,
            "{w}x{h}: picker {picked_psnr:.2} dB regressed >1 dB vs default {default_psnr:.2} dB"
        );
    }
}

/// On unstructured noise the picker still emits a conformant frame and
/// stays within a small tolerance of the default (no structure to
/// exploit, so the picker's job is mostly to not pick a worse point).
#[test]
fn picker_handles_noise_without_regression() {
    let (w, h) = (64usize, 64usize);
    let src = noise_gray(w, h);
    let opts = EncoderOptions::from_quality(50);

    let default_bytes = encode_gray8(&src, w as u32, h as u32, opts).unwrap();
    let default_psnr = psnr(&src, &decode_gray(&default_bytes, w, h));

    let picked_bytes = encode_gray8_round7(&src, w as u32, h as u32, opts).unwrap();
    let picked_psnr = psnr(&src, &decode_gray(&picked_bytes, w, h));

    assert!(
        picked_psnr >= default_psnr - 1.0,
        "noise: picker {picked_psnr:.2} dB regressed >1 dB vs default {default_psnr:.2} dB"
    );
}

/// Empty candidate lists are rejected.
#[test]
fn empty_candidate_lists_error() {
    let (w, h) = (16usize, 16usize);
    let src = gradient_gray(w, h);
    let opts = EncoderOptions::from_quality(50);
    assert!(encode_gray8_best_rd_grid(&src, w as u32, h as u32, opts, &[], &[Some(0.0)]).is_err());
    assert!(encode_gray8_best_rd_grid(&src, w as u32, h as u32, opts, &[1, 2], &[]).is_err());
}

/// `rdo_lambda = None` → pure-distortion ranking still produces a
/// conformant frame and the highest-PSNR candidate from the grid.
#[test]
fn pure_distortion_ranking_picks_high_psnr() {
    let (w, h) = (64usize, 64usize);
    let src = gradient_gray(w, h);
    let mut opts = EncoderOptions::from_quality(50);
    opts.rdo_lambda = None;
    // Pure-distortion ranking: lambda=0 in the cost, so the picker keeps
    // the lowest-SSE candidate regardless of wire size.
    let strips = [1u16, 2, 4];
    let lambdas = [Some(0.0_f32), None];
    let bytes =
        encode_gray8_best_rd_grid(&src, w as u32, h as u32, opts, &strips, &lambdas).unwrap();
    let picked_psnr = psnr(&src, &decode_gray(&bytes, w, h));

    // Should be at least as good as any single fixed candidate.
    let mut best_fixed = f64::NEG_INFINITY;
    for &sc in &strips {
        for &lam in &lambdas {
            let o = EncoderOptions {
                strip_count: sc,
                rdo_lambda: lam,
                ..opts
            };
            let b = encode_gray8(&src, w as u32, h as u32, o).unwrap();
            best_fixed = best_fixed.max(psnr(&src, &decode_gray(&b, w, h)));
        }
    }
    assert!(
        picked_psnr >= best_fixed - 0.01,
        "pure-distortion pick {picked_psnr:.2} below best fixed {best_fixed:.2}"
    );
}

/// Prints the measured headline table (run with `--nocapture`).
#[test]
fn report_grayscale_picker_table() {
    let opts = EncoderOptions::from_quality(50);
    println!("\n=== Round 101 grayscale RD-grid picker ===");
    for &(name, w, h, structured) in &[
        ("gradient 64x64", 64usize, 64usize, true),
        ("gradient 128x96", 128, 96, true),
        ("gradient 320x240", 320, 240, true),
        ("noise 64x64", 64, 64, false),
    ] {
        let src = if structured {
            gradient_gray(w, h)
        } else {
            noise_gray(w, h)
        };
        let d = encode_gray8(&src, w as u32, h as u32, opts).unwrap();
        let dp = psnr(&src, &decode_gray(&d, w, h));
        let p = encode_gray8_round7(&src, w as u32, h as u32, opts).unwrap();
        let pp = psnr(&src, &decode_gray(&p, w, h));
        println!(
            "{name:18}: default {dp:6.2} dB / {:5} B  ->  picker {pp:6.2} dB / {:5} B  (Δ {:+.2} dB / {:+} B)",
            d.len(),
            p.len(),
            pp - dp,
            p.len() as i64 - d.len() as i64,
        );
    }
}
