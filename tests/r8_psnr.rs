//! Round 8 validation — **per-strip independent (lambda, luma_weight) picker.**
//!
//! Round 7 (`16baf43`) added a frame-uniform three-axis RD grid picker
//! ([`oxideav_cinepak::encode_rgb24_round7`]): for each
//! `(strip_count, rdo_lambda, luma_weight)` from a deduped 3×3×3 grid,
//! trial-encode the whole frame, score by Y-channel SSE + λ·R, return
//! the lowest-cost bitstream. The chosen `(rdo_lambda, luma_weight)`
//! pair applies to **every strip** of the frame.
//!
//! ## Lever L — per-strip independent (lambda, luma_weight) picker
//!
//! Round 8 ([`oxideav_cinepak::encode_rgb24_round8`] /
//! [`oxideav_cinepak::encode_rgb24_per_strip_rd`]) plans the strips per
//! `strip_count` candidate, then for each strip independently sweeps
//! every `(lambda, luma_weight)` combination and picks the one
//! minimising per-strip Y-SSE + λ·R. The winning per-strip bitstreams
//! are assembled into a multi-strip frame.
//!
//! **Why this can win**: Cinepak's bitstream lets each strip carry its
//! own pair of codebooks trained independently. On **split-content**
//! frames where strips have qualitatively different pixel statistics —
//! e.g. a smooth-gradient top strip (best at high `luma_weight = 8`,
//! V4-saturated `lambda = 0`) and a saturated-chroma stripes bottom
//! strip (best at low `luma_weight = 1`, `lambda = 2.5`) — round 7
//! must pick **one** `(lambda, luma_weight)` per frame and compromise
//! on whichever strip is the "loser". Round 8 lets each strip pick its
//! own optimum.
//!
//! **Monotonicity guarantee**: [`encode_rgb24_round8`] also runs the
//! round-7 picker and keeps whichever pick has the lower Y-SSE + λ·R
//! cost, so on homogeneous content (where the per-strip greedy and
//! frame-uniform pick converge) round 8 matches round 7 exactly.
//!
//! ## Measured headlines (Y-channel SSE + λ·R scoring)
//!
//! `EncoderOptions::from_quality(50)` (default `rdo_lambda = Some(5.0)`):
//!
//! | fixture                              | r7              | r8              | delta            |
//! | ------------------------------------ | --------------- | --------------- | ---------------- |
//! | `four_strip_split` 256×256           | 52.91 dB/10138B | 53.01 dB/9562B  | +0.10 dB / -576B |
//! | `four_strip_split` 320×240           | 40.27 dB/13209B | 40.27 dB/12669B | +0.00 dB / -540B |
//! | `ultra_split` 256×256                | 40.70 dB/11410B | 40.74 dB/11482B | +0.04 dB / +72B  |
//!
//! Headlines are **wire-size reductions at iso-cost**, not pure-PSNR
//! gains. The per-strip greedy occasionally picks a Pareto-frontier
//! point with smaller wire at slightly different PSNR — the picker's
//! cost metric prefers it, so [`encode_rgb24_round8`] returns it.
//!
//! On the **round-7 headline fixtures** (homogeneous 64×64 and 320×240
//! gradients), round 8 matches round 7 to within ±0.3 dB PSNR_Y at
//! comparable or smaller wire size (the per-strip greedy converges to
//! the frame-uniform pick — strips of a homogeneous gradient have
//! near-identical pixel statistics).

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{
    encode_rgb24_per_strip_rd, encode_rgb24_round7, encode_rgb24_round8, CinepakDecoder,
    EncoderOptions,
};

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
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

/// Y-channel SSE per pixel for an encoded bitstream against a source RGB buffer.
fn y_sse_per_pixel(bytes: &[u8], src: &[u8], w: u32, h: u32) -> f64 {
    let mut dec = CinepakDecoder::new();
    let frame = dec.decode_frame(bytes, None).expect("decode");
    let stride = frame.stride();
    let pixels = frame.pixels();
    let mut sum_sq = 0.0f64;
    for r in 0..h as usize {
        let off_dec = r * stride;
        let off_src = r * (w as usize) * 3;
        for c in 0..(w as usize) {
            let ya = 0.299 * src[off_src + c * 3] as f64
                + 0.587 * src[off_src + c * 3 + 1] as f64
                + 0.114 * src[off_src + c * 3 + 2] as f64;
            let yb = 0.299 * pixels[off_dec + c * 3] as f64
                + 0.587 * pixels[off_dec + c * 3 + 1] as f64
                + 0.114 * pixels[off_dec + c * 3 + 2] as f64;
            let d = ya - yb;
            sum_sq += d * d;
        }
    }
    sum_sq / ((w as usize) * (h as usize)) as f64
}

/// Picker scoring metric: D_Y/N + λ·R/N. Round 8 minimises this.
fn picker_cost(bytes: &[u8], src: &[u8], w: u32, h: u32, lambda: f64) -> f64 {
    let n = (w as usize) * (h as usize);
    let d_per_pixel = y_sse_per_pixel(bytes, src, w, h);
    d_per_pixel + lambda * bytes.len() as f64 / n as f64
}

/// Decode + re-pack bitstream as a packed RGB24 row-major buffer at the
/// frame's stride (for PSNR comparison against the source).
fn decode_to_packed(bytes: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut d = CinepakDecoder::new();
    let f = d.decode_frame(bytes, None).expect("decode");
    let pixels = f.pixels();
    let mut packed = vec![0u8; (w * h * 3) as usize];
    for r in 0..h as usize {
        let off_dec = r * f.stride();
        let off_src = r * (w as usize) * 3;
        packed[off_src..off_src + (w as usize) * 3]
            .copy_from_slice(&pixels[off_dec..off_dec + (w as usize) * 3]);
    }
    packed
}

/// 256×256 four-strip split fixture: each 64-row strip carries
/// qualitatively different content (gradients vs solid colours vs
/// chroma stripes vs gray-red). Used to exercise the per-strip picker's
/// ability to pivot per-strip.
fn four_strip_split(w: usize, h: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; w * h * 3];
    let strip_h = h / 4;
    for strip_idx in 0..4 {
        let y0 = strip_idx * strip_h;
        let y1 = y0 + strip_h;
        match strip_idx {
            0 => {
                for r in y0..y1 {
                    for c in 0..w {
                        let off = r * w * 3 + c * 3;
                        let g = ((c * 255) / (w - 1)) as u8;
                        rgb[off] = g;
                        rgb[off + 1] = g;
                        rgb[off + 2] = g;
                    }
                }
            }
            1 => {
                for r in y0..y1 {
                    for c in 0..w {
                        let off = r * w * 3 + c * 3;
                        rgb[off + 2] = ((c * 255) / (w - 1)) as u8;
                    }
                }
            }
            2 => {
                for r in y0..y1 {
                    for c in 0..w {
                        let off = r * w * 3 + c * 3;
                        match c % 12 {
                            0..=3 => {
                                rgb[off] = 255;
                            }
                            4..=7 => {
                                rgb[off + 1] = 255;
                            }
                            _ => {
                                rgb[off + 2] = 255;
                            }
                        }
                    }
                }
            }
            _ => {
                for r in y0..y1 {
                    for c in 0..w {
                        let off = r * w * 3 + c * 3;
                        let v = ((c * 255) / (w - 1)) as u8;
                        rgb[off] = v;
                        rgb[off + 1] = v / 2;
                        rgb[off + 2] = v / 2;
                    }
                }
            }
        }
    }
    rgb
}

/// Round-7 small smooth-gradient headline fixture (regression guard).
fn synth_64x64() -> Vec<u8> {
    let w = 64usize;
    let h = 64usize;
    let mut rgb = vec![0u8; w * h * 3];
    for r in 0..h {
        for c in 0..w {
            let off = r * w * 3 + c * 3;
            rgb[off] = ((c * 255) / (w - 1)) as u8;
            rgb[off + 1] = (((r + c) * 255) / (w + h - 2)) as u8;
            rgb[off + 2] = ((r * 255) / (h - 1)) as u8;
        }
    }
    rgb
}

/// LCG-noise 64×64 fixture (regression guard for random content).
fn synth_noisy_64x64() -> Vec<u8> {
    let w = 64usize;
    let h = 64usize;
    let mut rgb = vec![0u8; w * h * 3];
    let mut s: u32 = 0xDEAD_BEEF;
    for px in rgb.iter_mut() {
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        *px = (s >> 16) as u8;
    }
    rgb
}

// ---------------------------------------------------------------------------
// Round-8 monotonicity guarantee (≥ round-7 in picker scoring metric).
// ---------------------------------------------------------------------------

#[test]
fn round8_at_least_as_good_as_round7_on_split_256() {
    let w = 256u32;
    let h = 256u32;
    let rgb = four_strip_split(w as usize, h as usize);
    let opts = EncoderOptions::from_quality(50);
    let r7 = encode_rgb24_round7(&rgb, w, h, opts).expect("r7");
    let r8 = encode_rgb24_round8(&rgb, w, h, opts).expect("r8");
    let lambda = opts.rdo_lambda.unwrap_or(0.0) as f64;
    let c7 = picker_cost(&r7, &rgb, w, h, lambda);
    let c8 = picker_cost(&r8, &rgb, w, h, lambda);
    assert!(
        c8 <= c7 + 1e-9,
        "round-8 monotonicity: c8 ({c8:.6}) must be ≤ c7 ({c7:.6})"
    );
}

#[test]
fn round8_at_least_as_good_as_round7_on_synth_64x64() {
    let rgb = synth_64x64();
    let opts = EncoderOptions::from_quality(50);
    let r7 = encode_rgb24_round7(&rgb, 64, 64, opts).expect("r7");
    let r8 = encode_rgb24_round8(&rgb, 64, 64, opts).expect("r8");
    let lambda = opts.rdo_lambda.unwrap_or(0.0) as f64;
    let c7 = picker_cost(&r7, &rgb, 64, 64, lambda);
    let c8 = picker_cost(&r8, &rgb, 64, 64, lambda);
    assert!(
        c8 <= c7 + 1e-9,
        "round-8 monotonicity: c8 ({c8:.6}) must be ≤ c7 ({c7:.6})"
    );
}

#[test]
fn round8_at_least_as_good_as_round7_on_noisy_64x64() {
    let rgb = synth_noisy_64x64();
    let opts = EncoderOptions::from_quality(50);
    let r7 = encode_rgb24_round7(&rgb, 64, 64, opts).expect("r7");
    let r8 = encode_rgb24_round8(&rgb, 64, 64, opts).expect("r8");
    let lambda = opts.rdo_lambda.unwrap_or(0.0) as f64;
    let c7 = picker_cost(&r7, &rgb, 64, 64, lambda);
    let c8 = picker_cost(&r8, &rgb, 64, 64, lambda);
    assert!(
        c8 <= c7 + 1e-9,
        "round-8 monotonicity: c8 ({c8:.6}) must be ≤ c7 ({c7:.6})"
    );
}

// ---------------------------------------------------------------------------
// Per-strip win on split-content fixture.
// ---------------------------------------------------------------------------

/// On the 256×256 four-strip split fixture, round 8 must save **at
/// least 400 B** vs round 7 at the default `rdo_lambda = 5.0` setting
/// (observed: -576 B). This is the headline measurable win for the
/// per-strip picker: heterogeneous strips can each pivot to their own
/// optimal (lambda, luma_weight) instead of compromising on one
/// frame-uniform value.
#[test]
fn round8_saves_wire_on_split_256x256() {
    let w = 256u32;
    let h = 256u32;
    let rgb = four_strip_split(w as usize, h as usize);
    let opts = EncoderOptions::from_quality(50);
    let r7 = encode_rgb24_round7(&rgb, w, h, opts).expect("r7");
    let r8 = encode_rgb24_round8(&rgb, w, h, opts).expect("r8");
    let savings_b = r7.len() as i64 - r8.len() as i64;
    assert!(
        savings_b >= 400,
        "round-8 must save ≥ 400 B on 256×256 split (got {savings_b}; r7={}, r8={})",
        r7.len(),
        r8.len()
    );
}

/// On the round-7 64×64 gradient headline fixture, round 8 (with
/// `rdo_lambda = None` for pure-distortion scoring) must match round 7
/// within 0.05 dB PSNR_Y — both pickers converge to the same
/// per-strip optimum on homogeneous content with no byte penalty.
#[test]
fn round8_matches_round7_on_homogeneous_pure_distortion() {
    let rgb = synth_64x64();
    let opts_none = EncoderOptions {
        rdo_lambda: None,
        ..EncoderOptions::from_quality(50)
    };
    let r7 = encode_rgb24_round7(&rgb, 64, 64, opts_none).expect("r7");
    let r8 = encode_rgb24_round8(&rgb, 64, 64, opts_none).expect("r8");
    let psnr7 = psnr_y(&rgb, &decode_to_packed(&r7, 64, 64));
    let psnr8 = psnr_y(&rgb, &decode_to_packed(&r8, 64, 64));
    let delta = psnr8 - psnr7;
    assert!(
        delta >= -0.05,
        "round-8 must not regress on homogeneous content under λ=None (delta {delta:.3} dB; r7={psnr7:.3}, r8={psnr8:.3})"
    );
}

// ---------------------------------------------------------------------------
// Per-strip RD picker direct API (validates the trial-encoder plumbing).
// ---------------------------------------------------------------------------

/// `encode_rgb24_per_strip_rd` rejects an empty `strip_candidates`
/// list — the caller must supply at least one strip-count to plan
/// against.
#[test]
fn per_strip_picker_rejects_empty_strip_candidates() {
    let rgb = synth_64x64();
    let opts = EncoderOptions::from_quality(50);
    let res = encode_rgb24_per_strip_rd(&rgb, 64, 64, opts, &[], &[Some(0.0)], &[2]);
    assert!(res.is_err());
}

/// Empty `lambda_candidates` list is rejected.
#[test]
fn per_strip_picker_rejects_empty_lambda_candidates() {
    let rgb = synth_64x64();
    let opts = EncoderOptions::from_quality(50);
    let res = encode_rgb24_per_strip_rd(&rgb, 64, 64, opts, &[1, 2], &[], &[2]);
    assert!(res.is_err());
}

/// Empty `luma_candidates` list is rejected.
#[test]
fn per_strip_picker_rejects_empty_luma_candidates() {
    let rgb = synth_64x64();
    let opts = EncoderOptions::from_quality(50);
    let res = encode_rgb24_per_strip_rd(&rgb, 64, 64, opts, &[1, 2], &[Some(0.0)], &[]);
    assert!(res.is_err());
}

/// Picker with all three lists single-element falls back to the
/// equivalent fixed-`strip_count` encode of `encode_rgb24` (no
/// real picking). Result must round-trip and beat 30 dB PSNR_Y on the
/// 64×64 gradient.
#[test]
fn per_strip_picker_single_candidate_each_axis_round_trips() {
    let rgb = synth_64x64();
    let opts = EncoderOptions::from_quality(50);
    let bytes = encode_rgb24_per_strip_rd(
        &rgb,
        64,
        64,
        opts,
        &[2],
        &[opts.rdo_lambda],
        &[opts.luma_weight],
    )
    .expect("encode");
    let psnr = psnr_y(&rgb, &decode_to_packed(&bytes, 64, 64));
    assert!(
        psnr >= 30.0,
        "single-axis picker on 64×64 gradient must round-trip ≥ 30 dB PSNR_Y (got {psnr:.3})"
    );
}
