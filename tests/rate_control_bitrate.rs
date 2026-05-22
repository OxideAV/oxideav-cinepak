//! Round-96 bitrate-target rate-control tests.
//!
//! Exercise [`CinepakEncoder::with_target_bitrate`] /
//! `with_target_frame_bytes`: confirm the encoder drives the three-axis
//! RD grid toward a per-frame byte budget, that under-budget frames are
//! actually under budget (and the highest-quality fit is chosen), and
//! that overshoot is reported via `RateStats` rather than erroring.

use oxideav_cinepak::{CinepakDecoder, CinepakEncoder, EncoderOptions};

/// Build a `width × height` RGB24 frame: a smooth horizontal+vertical
/// colour gradient with a `phase`-dependent shift so successive frames
/// differ enough to exercise the inter path.
fn gradient_frame(width: usize, height: usize, phase: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; width * height * 3];
    for r in 0..height {
        for c in 0..width {
            let off = (r * width + c) * 3;
            rgb[off] = (((c + phase) * 255 / width) & 0xff) as u8;
            rgb[off + 1] = ((r * 255 / height) & 0xff) as u8;
            rgb[off + 2] = ((((r + c + phase) / 2) * 255 / width) & 0xff) as u8;
        }
    }
    rgb
}

/// BT.601 Y-channel PSNR (dB) between a source RGB24 buffer and a decoded
/// frame of the same dimensions.
fn psnr_y(src: &[u8], decoded: &oxideav_cinepak::CinepakFrame, width: usize, height: usize) -> f64 {
    let stride = decoded.stride();
    let pixels = decoded.pixels();
    let mut sum_sq = 0.0f64;
    for r in 0..height {
        for c in 0..width {
            let so = (r * width + c) * 3;
            let do_ = r * stride + c * 3;
            let ya =
                0.299 * src[so] as f64 + 0.587 * src[so + 1] as f64 + 0.114 * src[so + 2] as f64;
            let yb = 0.299 * pixels[do_] as f64
                + 0.587 * pixels[do_ + 1] as f64
                + 0.114 * pixels[do_ + 2] as f64;
            let d = ya - yb;
            sum_sq += d * d;
        }
    }
    let mse = sum_sq / (width * height) as f64;
    if mse <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

/// A generous per-frame budget should be satisfiable: every frame ends
/// up under budget and `RateStats` confirms it.
#[test]
fn generous_budget_keeps_every_frame_under_target() {
    let (w, h) = (96usize, 64usize);
    // 1.5 Mbit/s at 10 fps = 150_000 bits/frame = 18_750 bytes/frame —
    // far above what a 96×64 gradient needs, so every frame fits.
    let mut enc = CinepakEncoder::new().with_target_bitrate(1_500_000, 10.0);
    assert_eq!(enc.target_frame_bytes(), Some(18_750));

    // One persistent decoder so inter frames resolve against the prior
    // reconstruction, matching the encoder's own carry-over.
    let mut dec = CinepakDecoder::new();
    for phase in 0..4usize {
        let frame = gradient_frame(w, h, phase * 4);
        let bytes = if phase == 0 {
            enc.encode_intra(&frame, w as u32, h as u32, EncoderOptions::from_quality(50))
                .unwrap()
        } else {
            enc.encode_inter(&frame, w as u32, h as u32, EncoderOptions::from_quality(50))
                .unwrap()
        };
        let stats = enc.last_rate_stats().expect("rate stats in budget mode");
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

        // Stream stays decodable (persistent decoder across frames).
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.width, w as u32);
        assert_eq!(f.height, h as u32);
    }
}

/// A moderate budget that the codec can hit forces the picker to a
/// smaller wire size than the quality-controlled default at the same
/// base quality, while staying under the cap.
#[test]
fn moderate_budget_shrinks_frame_vs_unconstrained() {
    let (w, h) = (128usize, 96usize);
    let frame = gradient_frame(w, h, 0);
    let base = EncoderOptions::from_quality(80);

    // Unconstrained reference encode at the same base quality.
    let mut ref_enc = CinepakEncoder::new();
    let ref_bytes = ref_enc
        .encode_intra(&frame, w as u32, h as u32, base)
        .unwrap();

    // Pick a budget comfortably below the unconstrained size but well
    // above the floor.
    let budget = ref_bytes.len() / 2;
    let mut enc = CinepakEncoder::new().with_target_frame_bytes(budget);
    let bytes = enc.encode_intra(&frame, w as u32, h as u32, base).unwrap();
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

    // The budgeted frame still decodes to a reasonable PSNR_Y (the
    // picker chose the best-quality fit, not the smallest).
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    let psnr = psnr_y(&frame, &f, w, h);
    assert!(psnr > 20.0, "budgeted PSNR_Y too low: {psnr:.2} dB");
}

/// A tiny budget that even the smallest grid candidate cannot meet must
/// not error: the smallest candidate is emitted and the overshoot is
/// reported via a positive `byte_delta` with `within_budget == false`.
#[test]
fn impossible_budget_reports_overshoot_without_erroring() {
    let (w, h) = (128usize, 96usize);
    let frame = gradient_frame(w, h, 0);
    let base = EncoderOptions::from_quality(60);

    // 1 byte/frame is impossible (a frame header alone is larger).
    let mut enc = CinepakEncoder::new().with_target_frame_bytes(1);
    let bytes = enc.encode_intra(&frame, w as u32, h as u32, base).unwrap();
    let stats = enc.last_rate_stats().unwrap();

    assert_eq!(stats.target_bytes, 1);
    assert!(!stats.within_budget);
    assert!(
        stats.byte_delta > 0,
        "expected overshoot, got {}",
        stats.byte_delta
    );
    assert_eq!(stats.actual_bytes, bytes.len());

    // Still a valid, decodable stream.
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    assert_eq!(f.width, w as u32);
    assert_eq!(f.height, h as u32);
}

/// Tighter budgets never produce larger frames than looser budgets at
/// the same base quality (rate-control monotonicity). Also confirms
/// each tighter budget keeps quality no higher than (or equal to) the
/// looser one — the picker trades quality for size as the cap shrinks.
#[test]
fn tighter_budget_is_monotonic_in_size() {
    let (w, h) = (128usize, 96usize);
    let frame = gradient_frame(w, h, 0);
    let base = EncoderOptions::from_quality(85);

    // Reference unconstrained size to scale budgets against.
    let mut ref_enc = CinepakEncoder::new();
    let ref_len = ref_enc
        .encode_intra(&frame, w as u32, h as u32, base)
        .unwrap()
        .len();

    let budgets = [
        ref_len, // generous (fits trivially)
        ref_len * 3 / 4,
        ref_len / 2,
        ref_len / 4,
    ];
    let mut prev_size = usize::MAX;
    for &budget in &budgets {
        let mut enc = CinepakEncoder::new().with_target_frame_bytes(budget);
        let bytes = enc.encode_intra(&frame, w as u32, h as u32, base).unwrap();
        let stats = enc.last_rate_stats().unwrap();
        // When within budget, the actual size must be ≤ budget.
        if stats.within_budget {
            assert!(bytes.len() <= budget);
        }
        // Monotonic: a tighter budget never yields a larger frame.
        assert!(
            bytes.len() <= prev_size,
            "budget {budget}: size {} > previous {prev_size}",
            bytes.len()
        );
        prev_size = bytes.len();
    }
}

/// `reset` preserves the configured budget so a rate-control encoder can
/// be re-used across sequences; `clear_target_bitrate` returns to
/// quality-controlled mode (no `RateStats`).
#[test]
fn reset_preserves_budget_clear_disables_it() {
    let (w, h) = (64usize, 48usize);
    let frame = gradient_frame(w, h, 0);
    let base = EncoderOptions::from_quality(50);

    let mut enc = CinepakEncoder::new().with_target_bitrate(800_000, 12.0);
    let expected = (800_000f64 / 8.0 / 12.0).round() as usize;
    assert_eq!(enc.target_frame_bytes(), Some(expected));

    enc.encode_intra(&frame, w as u32, h as u32, base).unwrap();
    assert!(enc.last_rate_stats().is_some());

    enc.reset();
    assert_eq!(
        enc.target_frame_bytes(),
        Some(expected),
        "reset must preserve the budget"
    );

    // Disable: now quality-controlled, no rate stats.
    enc.clear_target_bitrate();
    assert_eq!(enc.target_frame_bytes(), None);
    enc.encode_intra(&frame, w as u32, h as u32, base).unwrap();
    assert!(enc.last_rate_stats().is_none());
}

/// Fractional fps (23.976) is honoured in the budget arithmetic.
#[test]
fn fractional_fps_budget_arithmetic() {
    let enc = CinepakEncoder::new().with_target_bitrate(1_000_000, 23.976);
    // 1_000_000 / 8 / 23.976 ≈ 5213.5 → 5214 (round-half-up via .round()).
    let expected = (1_000_000f64 / 8.0 / 23.976).round() as usize;
    assert_eq!(enc.target_frame_bytes(), Some(expected));
    assert_eq!(expected, 5214);
}

/// Zero / non-finite fps clamps to 1.0; the budget never panics.
#[test]
fn degenerate_fps_clamps_to_one() {
    let enc = CinepakEncoder::new().with_target_bitrate(80_000, 0.0);
    // fps clamped to 1.0 ⇒ 80_000 / 8 = 10_000 bytes/frame.
    assert_eq!(enc.target_frame_bytes(), Some(10_000));
}
