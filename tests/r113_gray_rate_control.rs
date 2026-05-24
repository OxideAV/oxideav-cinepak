//! Round 113 — target-bitrate rate control on the grayscale stateful
//! inter path.
//!
//! Round 96 added per-frame byte-budget rate control to the colour
//! ([`CinepakEncoder::encode_intra`] / [`encode_inter`]) path: the encoder
//! sweeps an RD grid around the caller's `EncoderOptions` and commits the
//! highest-quality candidate whose encoded size fits a per-frame budget
//! (`bits_per_second / 8 / fps`), reporting adherence via
//! [`CinepakEncoder::last_rate_stats`] and never erroring on overshoot.
//! That path was hardcoded to the `Yuv12` (colour) pixel pipeline and
//! BT.601 Y-channel SSE scoring, so the round-104 grayscale stateful inter
//! path ([`encode_intra_gray8`] / [`encode_inter_gray8`]) explicitly used
//! the caller's `opts` verbatim regardless of any configured budget.
//!
//! Round 113 threads `PixelMode` through the round-96 budget worker and
//! routes the grayscale entry points through it when a budget is
//! configured. The grayscale grid is a two-axis `(strip_count,
//! rdo_lambda)` cross-product — the `luma_weight` axis is dropped because
//! for `Gray8` codebook entries the distance metric scales all four Y dims
//! by `luma_weight`, a uniform positive scale that leaves every clustering
//! decision invariant (same rationale as the round-101 grayscale picker).
//! Scoring is direct-luma SSE (the 8-bit luminance *is* the Y channel).
//!
//! Wire-format reference: spec §3.4 / §4 of
//! `docs/video/cinepak/spec/02-codebooks.md` (codebook chunk taxonomy,
//! including the `0x24xx` / `0x26xx` grayscale families) and §3 of
//! `03-vectors-and-macroblocks.md` (the `0x3100` inter vector chunk).
//! Rate control is encoder policy, not a bitstream feature — decoded
//! output is conformant Cinepak.

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{CinepakDecoder, CinepakEncoder, CinepakPixelFormat, EncoderOptions};

/// Build a `w × h` grayscale frame: a smooth diagonal luma gradient with a
/// `phase`-dependent shift so successive frames differ enough to exercise
/// the inter path.
fn gradient_gray(w: usize, h: usize, phase: usize) -> Vec<u8> {
    let mut gray = vec![0u8; w * h];
    for r in 0..h {
        for c in 0..w {
            gray[r * w + c] = ((((r + c + phase) * 255) / (w + h - 2)) & 0xff) as u8;
        }
    }
    gray
}

/// Direct-luma PSNR (dB) between a source `Gray8` plane and a decoded
/// grayscale frame of the same dimensions.
fn psnr_gray(
    src: &[u8],
    decoded: &oxideav_cinepak::CinepakFrame,
    width: usize,
    height: usize,
) -> f64 {
    assert_eq!(decoded.pixel_format, CinepakPixelFormat::Gray8);
    let stride = decoded.stride();
    let pixels = decoded.pixels();
    let mut sum_sq = 0.0f64;
    for r in 0..height {
        for c in 0..width {
            let d = pixels[r * stride + c] as f64 - src[r * width + c] as f64;
            sum_sq += d * d;
        }
    }
    let mse = sum_sq / (width * height) as f64;
    if mse <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

/// A generous per-frame budget should be satisfiable across an intra +
/// inter grayscale sequence: every frame ends up under budget and
/// `RateStats` confirms it, while the stream stays decodable.
#[test]
fn generous_budget_keeps_every_gray_frame_under_target() {
    let (w, h) = (96usize, 64usize);
    // 1.5 Mbit/s at 10 fps = 18_750 bytes/frame — far above what a 96×64
    // grayscale gradient needs, so every frame fits.
    let mut enc = CinepakEncoder::new().with_target_bitrate(1_500_000, 10.0);
    assert_eq!(enc.target_frame_bytes(), Some(18_750));

    let mut dec = CinepakDecoder::new();
    for phase in 0..4usize {
        let frame = gradient_gray(w, h, phase * 4);
        let bytes = if phase == 0 {
            enc.encode_intra_gray8(&frame, w as u32, h as u32, EncoderOptions::from_quality(50))
                .unwrap()
        } else {
            enc.encode_inter_gray8(&frame, w as u32, h as u32, EncoderOptions::from_quality(50))
                .unwrap()
        };
        let stats = enc
            .last_rate_stats()
            .expect("rate stats in grayscale budget mode");
        assert_eq!(stats.target_bytes, 18_750);
        assert_eq!(stats.actual_bytes, bytes.len());
        assert!(
            stats.within_budget,
            "phase {phase}: {} > budget {}",
            stats.actual_bytes, stats.target_bytes
        );
        assert!(
            stats.byte_delta <= 0,
            "phase {phase}: delta {}",
            stats.byte_delta
        );
        assert!(stats.trials >= 1);
        assert!(bytes.len() <= 18_750);

        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.pixel_format, CinepakPixelFormat::Gray8);
        assert_eq!(f.width, w as u32);
        assert_eq!(f.height, h as u32);
        // The budgeted frame still decodes to a reasonable PSNR.
        let psnr = psnr_gray(&frame, &f, w, h);
        assert!(
            psnr > 20.0,
            "phase {phase}: budgeted PSNR {psnr:.2} dB too low"
        );
    }
}

/// A moderate budget the codec can hit forces the picker to a smaller wire
/// size than the quality-controlled grayscale default at the same base
/// quality, while staying under the cap.
#[test]
fn moderate_budget_shrinks_gray_frame_vs_unconstrained() {
    let (w, h) = (128usize, 96usize);
    let frame = gradient_gray(w, h, 0);
    let base = EncoderOptions::from_quality(80);

    // Unconstrained reference encode at the same base quality.
    let mut ref_enc = CinepakEncoder::new();
    let ref_bytes = ref_enc
        .encode_intra_gray8(&frame, w as u32, h as u32, base)
        .unwrap();

    let budget = ref_bytes.len() / 2;
    let mut enc = CinepakEncoder::new().with_target_frame_bytes(budget);
    let bytes = enc
        .encode_intra_gray8(&frame, w as u32, h as u32, base)
        .unwrap();
    let stats = enc.last_rate_stats().unwrap();

    assert_eq!(stats.target_bytes, budget);
    assert!(
        stats.within_budget,
        "budget {budget} not met: actual {}",
        stats.actual_bytes
    );
    assert!(
        bytes.len() <= budget,
        "actual {} > budget {budget}",
        bytes.len()
    );
    assert!(
        bytes.len() < ref_bytes.len(),
        "budget encode {} not smaller than unconstrained {}",
        bytes.len(),
        ref_bytes.len()
    );

    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    assert_eq!(f.pixel_format, CinepakPixelFormat::Gray8);
    let psnr = psnr_gray(&frame, &f, w, h);
    assert!(psnr > 20.0, "budgeted PSNR too low: {psnr:.2} dB");
}

/// A tiny budget even the smallest grayscale grid candidate cannot meet
/// must not error: the smallest candidate is emitted and the overshoot is
/// reported via a positive `byte_delta` with `within_budget == false`.
#[test]
fn impossible_gray_budget_reports_overshoot_without_erroring() {
    let (w, h) = (128usize, 96usize);
    let frame = gradient_gray(w, h, 0);
    let base = EncoderOptions::from_quality(60);

    let mut enc = CinepakEncoder::new().with_target_frame_bytes(1);
    let bytes = enc
        .encode_intra_gray8(&frame, w as u32, h as u32, base)
        .unwrap();
    let stats = enc.last_rate_stats().unwrap();

    assert_eq!(stats.target_bytes, 1);
    assert!(!stats.within_budget);
    assert!(
        stats.byte_delta > 0,
        "expected overshoot, got {}",
        stats.byte_delta
    );
    assert_eq!(stats.actual_bytes, bytes.len());

    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    assert_eq!(f.pixel_format, CinepakPixelFormat::Gray8);
    assert_eq!(f.width, w as u32);
    assert_eq!(f.height, h as u32);
}

/// Tighter budgets never produce larger grayscale frames than looser
/// budgets at the same base quality (rate-control monotonicity).
#[test]
fn tighter_gray_budget_is_monotonic_in_size() {
    let (w, h) = (128usize, 96usize);
    let frame = gradient_gray(w, h, 0);
    let base = EncoderOptions::from_quality(85);

    let mut ref_enc = CinepakEncoder::new();
    let ref_len = ref_enc
        .encode_intra_gray8(&frame, w as u32, h as u32, base)
        .unwrap()
        .len();

    let budgets = [ref_len, ref_len * 3 / 4, ref_len / 2, ref_len / 4];
    let mut prev_size = usize::MAX;
    for &budget in &budgets {
        let mut enc = CinepakEncoder::new().with_target_frame_bytes(budget);
        let bytes = enc
            .encode_intra_gray8(&frame, w as u32, h as u32, base)
            .unwrap();
        let stats = enc.last_rate_stats().unwrap();
        if stats.within_budget {
            assert!(bytes.len() <= budget);
        }
        assert!(
            bytes.len() <= prev_size,
            "budget {budget}: size {} > previous {prev_size}",
            bytes.len()
        );
        prev_size = bytes.len();
    }
}

/// Budget mode rides the stateful carry-over: inter frames stay budgeted
/// and skip / selectively-update against the committed previous grayscale
/// reconstruction. A static fixture under a generous budget should drive
/// the inter tail toward chunk-omission (small, all-SKIP frames) while
/// every frame stays under budget and decodes losslessly-ish.
#[test]
fn gray_inter_budget_rides_carryover_on_static_fixture() {
    let (w, h) = (64usize, 48usize);
    // Static four-band fixture so the rolling codebook stabilises and the
    // inter tail collapses toward SKIP.
    let mut frame = vec![0u8; w * h];
    let lumas = [40u8, 110, 180, 230];
    for r in 0..h {
        for c in 0..w {
            frame[r * w + c] = lumas[(r / (h / 4)).min(3)];
        }
    }
    let base = EncoderOptions::from_quality(50);

    // Generous budget so the picker can always fit.
    let mut enc = CinepakEncoder::new().with_target_frame_bytes(20_000);
    let mut dec = CinepakDecoder::new();

    let intra = enc
        .encode_intra_gray8(&frame, w as u32, h as u32, base)
        .unwrap();
    let s0 = enc.last_rate_stats().unwrap();
    assert!(s0.within_budget);
    let f0 = dec.decode_frame(&intra, None).unwrap();
    assert!(psnr_gray(&frame, &f0, w, h) > 24.0);

    let mut last_inter_len = usize::MAX;
    for _ in 0..5 {
        let inter = enc
            .encode_inter_gray8(&frame, w as u32, h as u32, base)
            .unwrap();
        let s = enc.last_rate_stats().unwrap();
        assert!(
            s.within_budget,
            "inter frame over budget: {}",
            s.actual_bytes
        );
        let f = dec.decode_frame(&inter, None).unwrap();
        assert_eq!(f.pixel_format, CinepakPixelFormat::Gray8);
        assert!(psnr_gray(&frame, &f, w, h) > 24.0);
        last_inter_len = inter.len();
    }
    // Once the codebook stabilises the static inter frame is tiny relative
    // to the intra keyframe (chunk-omission / all-SKIP).
    assert!(
        last_inter_len < intra.len(),
        "static inter {last_inter_len} not smaller than intra {}",
        intra.len()
    );
}

/// The grayscale budget grid drops the no-op `luma_weight` axis, so it runs
/// at most 9 trials (3 strip counts × 3 lambdas) — strictly fewer than the
/// colour grid's up-to-27. Verified indirectly via `RateStats::trials`: a
/// generous budget exercises every candidate, and the count must be ≤ 9.
#[test]
fn gray_budget_grid_omits_luma_axis() {
    let (w, h) = (64usize, 64usize);
    let frame = gradient_gray(w, h, 0);
    // Generous budget so every candidate is trial-encoded.
    let mut enc = CinepakEncoder::new().with_target_frame_bytes(1_000_000);
    enc.encode_intra_gray8(&frame, w as u32, h as u32, EncoderOptions::from_quality(50))
        .unwrap();
    let stats = enc.last_rate_stats().unwrap();
    assert!(
        stats.trials >= 1 && stats.trials <= 9,
        "grayscale grid should be ≤ 9 trials (no luma axis), got {}",
        stats.trials
    );
}

/// `reset` preserves the configured budget so a grayscale rate-control
/// encoder can be reused across sequences; `clear_target_bitrate` returns
/// to quality-controlled mode (no `RateStats`).
#[test]
fn reset_preserves_budget_clear_disables_it_gray() {
    let (w, h) = (64usize, 48usize);
    let frame = gradient_gray(w, h, 0);
    let base = EncoderOptions::from_quality(50);

    let mut enc = CinepakEncoder::new().with_target_bitrate(800_000, 12.0);
    let expected = (800_000f64 / 8.0 / 12.0).round() as usize;
    assert_eq!(enc.target_frame_bytes(), Some(expected));

    enc.encode_intra_gray8(&frame, w as u32, h as u32, base)
        .unwrap();
    assert!(enc.last_rate_stats().is_some());

    enc.reset();
    assert_eq!(
        enc.target_frame_bytes(),
        Some(expected),
        "reset must preserve the budget"
    );

    enc.clear_target_bitrate();
    assert_eq!(enc.target_frame_bytes(), None);
    // Quality-controlled mode: the caller's opts are used verbatim, no
    // rate stats.
    enc.encode_intra_gray8(&frame, w as u32, h as u32, base)
        .unwrap();
    assert!(enc.last_rate_stats().is_none());
}
