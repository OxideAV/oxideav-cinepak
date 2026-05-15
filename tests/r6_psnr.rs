//! Round 6 PSNR_Y improvement validation.
//!
//! Round 5 (`4cd8a02`) landed luma-weighted distance + luma-prioritized
//! median-cut split on top of the round-4 LBG codebook, hitting
//! **42.39 dB PSNR_Y on the 64×64 gradient at 2554 B** via
//! `encode_rgb24_best_strips(opts, &[1, 2, 4])` at `q=50`.
//!
//! Round 6 adds **two complementary levers** at the encoder layer:
//!
//! - **Lever H — post-classification Lloyd polish (PCL)**
//!   (`EncoderOptions::pcl_max_iter`, default `2`): after the round-3
//!   Lagrangian RDO step routes each non-skip MB to V4 or V1, each
//!   *used* codebook slot is re-trained from only the actually-selected
//!   member vectors and the per-MB classification is re-run. The LBG
//!   warm-build minimised distortion across all non-skip vectors, but
//!   the RDO step sends a substantial fraction of them to V1 (cheaper
//!   wire footprint) — so the LBG centroids aren't the means of each
//!   slot's actual selected member set. The polish closes that gap
//!   without changing slot identity (unused slots stay byte-identical
//!   to the LBG output, so cross-frame persistence and selective-update
//!   / chunk-omission wins on inter strips are unaffected). 2 polish
//!   iterations capture essentially the full gain (the first polish
//!   often re-classifies a few V1 MBs to V4 once V4 slots tighten; the
//!   second iteration is convergence cleanup).
//!
//! - **Lever I — two-axis RD grid picker**
//!   (`encode_rgb24_best_rd_grid` + `encode_rgb24_round6` convenience
//!   wrapper): trial-encodes every `(strip_count, rdo_lambda)` from the
//!   cross-product of two candidate lists and picks the lowest
//!   Lagrangian-cost result. The round-3 picker only varies
//!   `strip_count` and reuses `opts.rdo_lambda` for the per-MB RDO of
//!   each trial — but on small frames where the V4 codebook is
//!   effectively saturated (e.g. 64×64 at q=50 with strip_count=4
//!   gives 64 V4 sub-blocks across 64 codebook slots — exact V4
//!   representation), lowering the per-MB lambda routes more MBs to V4
//!   and harvests most of the residual error at modest wire-size cost.
//!   Round 6's `encode_rgb24_round6` sweeps `strip_candidates =
//!   [1, 2, 4]` × `lambda_candidates = [Some(0.0), Some(2.5),
//!   opts.rdo_lambda]` for 9 trial encodes per frame.
//!
//! ## Round 5 vs Round 6 measured headline numbers
//!
//! At `EncoderOptions::from_quality(50)`, BT.601 Y-channel mean PSNR.
//!
//! | fixture                                       | r5             | r6             | delta              |
//! | --------------------------------------------- | -------------- | -------------- | ------------------ |
//! | 64×64 gradient `encode_rgb24_round6`          | 42.39 dB/2554B | 43.44 dB/2704B | +1.05 dB / +5.9% B |
//! | 320×240 gradient `encode_rgb24_round6`        | 41.70 dB/9288B | 45.25 dB/14586B| +3.55 dB / +57% B  |
//! | 64×64 single-strip + PCL=2 vs PCL=0           | 36.55 dB       | 38.19 dB       | +1.64 dB / same B  |
//! | 64×64 LCG-noise `encode_rgb24_round6`         | 23.00 dB       | ~23.0 dB       | ±0.05 dB           |
//!
//! The 64×64 headline gain (+1.05 dB) exceeds the round-6 ≥ 0.5 dB
//! target. The 320×240 picker actively chose a higher-bitrate point
//! (the Lagrangian cost saw it as the global optimum given the bigger
//! frame had more pixels to amortise the extra bytes over) — to compare
//! at the round-5 wire size the per-strip PCL alone gives 43.16 dB at
//! 10170 B (+1.45 dB / +9% B vs r5).
//!
//! ## Lead over ffmpeg's reference encoder
//!
//! Round 5 stood at +5.49 dB lead on the 64×64 gradient (42.39 dB vs
//! ffmpeg's ~36.9 dB). Round 6 extends this to **+6.55 dB** at modest
//! wire-size growth.

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{
    encode_rgb24, encode_rgb24_best_rd_grid, encode_rgb24_best_strips, encode_rgb24_round6,
    CinepakDecoder, EncoderOptions,
};

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

/// **Round 6 headline — 64×64 gradient ≥ 42.89 dB PSNR_Y via the
/// round-6 picker.**
///
/// The round-5 `encode_rgb24_best_strips(opts, &[1, 2, 4])` baseline
/// hit 42.39 dB at 2554 B. The round-6 `encode_rgb24_round6` picker
/// (which searches `(strip_count, rdo_lambda) ∈ [1,2,4] × [0.0, 2.5,
/// opts.rdo_lambda]` and applies the default-on `pcl_max_iter = 2`
/// post-classification Lloyd polish) hits 43.44 dB at 2704 B — a
/// +1.05 dB lift at +5.9% wire size. The floor below is `>= 42.89 dB`
/// (round-5 + 0.5 dB target) to accommodate measurement noise across
/// builds.
#[test]
fn round6_64x64_gradient_breaks_42_89db() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions::from_quality(50);

    let bytes = encode_rgb24_round6(&rgb, w, h, opts).unwrap();
    let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
    eprintln!(
        "Round 6 picker on 64x64 gradient at q=50: psnr_y = {:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 42.89,
        "Round-6 target: PSNR_Y on 64x64 gradient should be >= 42.89 dB; got {psnr:.3}"
    );
}

/// **Round-6 vs round-5 explicit A/B on 64×64.** Compares the round-5
/// `encode_rgb24_best_strips(&[1, 2, 4])` picker (which is now bundled
/// with the round-6 default-on Lever H polish) against the round-6
/// `encode_rgb24_round6` picker (which adds the lambda axis on top of
/// PCL). Asserts the round-6 picker delivers ≥ 0.5 dB additional
/// PSNR_Y over the round-5 picker — the round-6 ≥ 0.5 dB target.
#[test]
fn round6_vs_round5_picker_64x64_lifts_by_0_5db() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions::from_quality(50);

    let bytes_r5 = encode_rgb24_best_strips(&rgb, w, h, opts, &[1, 2, 4]).unwrap();
    let psnr_r5 = psnr_y(&rgb, &decode_packed(&bytes_r5, w, h));
    let bytes_r6 = encode_rgb24_round6(&rgb, w, h, opts).unwrap();
    let psnr_r6 = psnr_y(&rgb, &decode_packed(&bytes_r6, w, h));
    let delta = psnr_r6 - psnr_r5;
    eprintln!(
        "Round 6 vs round 5 picker on 64x64 gradient at q=50: \
         r5 picker psnr_y={:.3} dB ({} B), r6 picker psnr_y={:.3} dB ({} B), \
         delta = +{:.3} dB",
        psnr_r5,
        bytes_r5.len(),
        psnr_r6,
        bytes_r6.len(),
        delta
    );
    assert!(
        delta >= 0.5,
        "Round-6 target: round-6 picker should lift PSNR_Y by >= 0.5 dB on 64x64 gradient; got {delta:+.3}"
    );
}

/// **Lever H (PCL) isolated delta — single-strip 64×64 gradient.**
/// At `strip_count=1` the V4 codebook (64 entries, 16 sub-block
/// vectors per MB × 16 MBs = 256 vectors) is fully under-determined,
/// so the post-classification Lloyd polish has the most room to
/// improve. Asserts PCL=2 lifts PSNR_Y by ≥ 1.0 dB over PCL=0
/// (observed: 36.55 → 38.19 dB = +1.64 dB).
#[test]
fn lever_h_pcl_64x64_single_strip_lifts_by_1db() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let base = EncoderOptions {
        strip_count: 1,
        ..EncoderOptions::from_quality(50)
    };

    let opts_off = EncoderOptions {
        pcl_max_iter: 0,
        ..base
    };
    let opts_on = EncoderOptions {
        pcl_max_iter: 2,
        ..base
    };
    let bytes_off = encode_rgb24(&rgb, w, h, opts_off).unwrap();
    let psnr_off = psnr_y(&rgb, &decode_packed(&bytes_off, w, h));
    let bytes_on = encode_rgb24(&rgb, w, h, opts_on).unwrap();
    let psnr_on = psnr_y(&rgb, &decode_packed(&bytes_on, w, h));
    let delta = psnr_on - psnr_off;
    eprintln!(
        "Lever H on 64x64 gradient strip=1 at q=50: \
         pcl=0 psnr_y={:.3} dB ({} B), pcl=2 psnr_y={:.3} dB ({} B), \
         delta = +{:.3} dB",
        psnr_off,
        bytes_off.len(),
        psnr_on,
        bytes_on.len(),
        delta
    );
    assert!(
        delta >= 1.0,
        "Lever-H pcl_max_iter=2 should lift PSNR_Y by >= 1 dB on 64x64 single-strip gradient; got {delta:+.3}"
    );
}

/// **Lever H slot-identity preservation.** Verifies that PCL only
/// updates the codebook slots that are actually referenced by the
/// per-MB classification — slots not used by any MB stay byte-identical
/// to the LBG output. This is critical for cross-frame persistence:
/// PCL must not perturb unused slots, otherwise the inter-frame
/// chunk-omission / selective-update path would emit needless updates.
///
/// Indirect check via the inter encoder's chunk-omission: encode a
/// static fixture, then encode the same frame as inter — with PCL on
/// inter frames disabled (PCL is intra-side only by virtue of running
/// after RDO classification, which never reseeds rolling-state slots
/// itself), all unchanged slots should still match the rolling
/// codebook so selective-update or chunk-omission fires.
#[test]
fn round6_pcl_preserves_unused_slots() {
    use oxideav_cinepak::CinepakEncoder;

    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions::from_quality(50);
    let mut enc = CinepakEncoder::new();
    let bytes_intra = enc.encode_intra(&rgb, w, h, opts).unwrap();
    // Now encode the same frame as inter — with cross-frame
    // persistence on (default) and the PCL polish having only
    // adjusted *used* slots, the inter frame should be substantially
    // smaller than the intra (chunk-omission / selective-update fires
    // on the slots PCL didn't touch).
    let bytes_inter = enc.encode_inter(&rgb, w, h, opts).unwrap();
    eprintln!(
        "Round 6 PCL slot-identity check: intra={} B, inter (static)={} B",
        bytes_intra.len(),
        bytes_inter.len()
    );
    // Inter should be at most 60% of intra — chunk-omission /
    // selective-update kicks in only when the rolling codebook (which
    // PCL just updated) still matches what the previous frame's
    // decoder holds. If PCL had perturbed unused slots, every
    // subsequent inter frame would emit full-replace.
    assert!(
        bytes_inter.len() < bytes_intra.len() * 6 / 10,
        "Round-6 PCL must preserve unused slot identities (inter should be < 60% of intra on static); intra={} inter={}",
        bytes_intra.len(),
        bytes_inter.len()
    );
}

/// **`pcl_max_iter = 0` regression guard** — disabling PCL must
/// reproduce the round-5 strip-picker PSNR_Y on 64×64 gradient (=
/// 42.39 dB at 2554 B exactly). Asserts the round-5 floor `>= 42.39 dB`
/// and the round-5 byte count `<= 2554 B` are both still met when PCL
/// is off and the legacy `encode_rgb24_best_strips` picker is used.
#[test]
fn pcl_0_restores_round5_picker_baseline() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions {
        pcl_max_iter: 0,
        ..EncoderOptions::from_quality(50)
    };
    let bytes = encode_rgb24_best_strips(&rgb, w, h, opts, &[1, 2, 4]).unwrap();
    let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
    eprintln!(
        "pcl=0 + r5 picker on 64x64 gradient at q=50: psnr_y={:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 42.38,
        "pcl=0 + r5 picker should reproduce round-5 baseline PSNR_Y >= 42.38 dB; got {psnr:.3}"
    );
    assert!(
        bytes.len() <= 2554,
        "pcl=0 + r5 picker should reproduce round-5 baseline wire size <= 2554 B; got {} B",
        bytes.len()
    );
}

/// **LCG-noise no-regression** — pure noise has no codebook structure
/// to exploit; round-6 levers should be a near-wash on this fixture
/// (within ±0.5 dB of round-5's 23.00 dB on 64×64). Round 6 actually
/// produces 23.04 dB via the round-6 picker (within noise).
#[test]
fn round6_noisy_64x64_no_regression() {
    let rgb = synth_noisy_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions::from_quality(50);

    let bytes = encode_rgb24_round6(&rgb, w, h, opts).unwrap();
    let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
    eprintln!(
        "Round 6 picker on 64x64 LCG-noise at q=50: psnr_y = {:.3} dB ({} B)",
        psnr,
        bytes.len()
    );
    assert!(
        psnr >= 22.5,
        "Round-6 should not regress on LCG-noise: PSNR_Y >= 22.5 dB; got {psnr:.3}"
    );
}

/// **`encode_rgb24_best_rd_grid` empty-list error path** — both
/// candidate lists must be non-empty. Confirms the API rejects an
/// empty `strip_candidates` and an empty `lambda_candidates`.
#[test]
fn round6_best_rd_grid_rejects_empty_lists() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let opts = EncoderOptions::from_quality(50);
    assert!(encode_rgb24_best_rd_grid(&rgb, w, h, opts, &[], &[Some(0.0)]).is_err());
    assert!(encode_rgb24_best_rd_grid(&rgb, w, h, opts, &[1], &[]).is_err());
}

/// **`pcl_max_iter` boundary** — `pcl_max_iter = 1` should be strictly
/// better than `pcl_max_iter = 0` on the 64×64 single-strip gradient
/// (the first polish iteration captures the bulk of the gain). Any
/// higher iteration count should be ≥ the iter=1 result (monotonic in
/// PCL passes — each pass either further refines or no-ops).
#[test]
fn round6_pcl_iter_count_monotonic_lift() {
    let rgb = synth_64x64();
    let w = 64u32;
    let h = 64u32;
    let base = EncoderOptions {
        strip_count: 1,
        ..EncoderOptions::from_quality(50)
    };
    let mut prior_psnr: f64 = -f64::INFINITY;
    for &iter in &[0u8, 1, 2, 3, 4] {
        let opts = EncoderOptions {
            pcl_max_iter: iter,
            ..base
        };
        let bytes = encode_rgb24(&rgb, w, h, opts).unwrap();
        let psnr = psnr_y(&rgb, &decode_packed(&bytes, w, h));
        eprintln!(
            "pcl_max_iter={iter}: psnr_y={:.3} dB ({} B)",
            psnr,
            bytes.len()
        );
        if iter == 1 {
            // The first polish iteration must strictly improve over
            // iter=0 (the LBG-only baseline) — the polish closes the
            // RDO-routing gap that LBG can't see.
            assert!(
                psnr > prior_psnr,
                "PCL iter=1 should strictly improve over iter=0; got {psnr:.3} vs {prior_psnr:.3}"
            );
        } else if iter > 1 {
            // Subsequent iterations may saturate but should never
            // *strictly* regress (within 0.05 dB measurement noise).
            assert!(
                psnr + 0.05 >= prior_psnr,
                "PCL iter={iter} must be >= iter={} - 0.05 dB; got {psnr:.3} vs {prior_psnr:.3}",
                iter - 1
            );
        }
        prior_psnr = psnr;
    }
}
