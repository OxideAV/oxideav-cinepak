//! Round-7 encoder behaviour tests.
//!
//! Covers:
//!
//! 1. **Empty-cluster slot reclamation** — exposes
//!    `EncoderOptions::stale_slot_threshold`. Drives the encoder past
//!    the threshold with a fixture where many codebook slots go
//!    unreferenced (a static prefix frame stretched across enough
//!    inter frames to trip the per-frame staleness counter), then
//!    cuts to new content. Asserts:
//!    (a) reclamation fires (`last_frame_stats().reclaimed_*_slots >
//!        0` on the post-threshold inter frame),
//!    (b) `forced_full_chunks > 0` on the reclamation frame,
//!    (c) the chain still decodes coherently,
//!    (d) `stale_slot_threshold = None` disables the path entirely.
//!
//! 2. **Adaptive bisection tolerance**
//!    (`TwoPassRateControl::encode_at_target_window_bytes_adaptive`).
//!    Drives a small slow-pan-then-cut sequence with min/max
//!    tolerance bounds and confirms:
//!    (a) the chain decodes,
//!    (b) the byte total stays within a bounded drift of target.
//!
//! 3. **last_frame_stats reset semantics** — verifies counters reset
//!    at the start of each frame (an intra-after-inter resets to all
//!    zeros even if the prior inter triggered reclamation).
//!
//! Wire-format reference: spec §3.4 / §4 / §5 of `02-codebooks.md`.

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{
    CinepakDecoder, CinepakEncoder, EncoderOptions, RateControlledFrame, TwoPassRateControl,
};

/// Build a 32×32 static fixture: solid mid-gray with a 4×4 distinctive
/// block at the top-left to give the codebook a small "hot region"
/// that gets used while every other slot decays into staleness.
fn static_with_hot_corner(width: usize, height: usize) -> Vec<u8> {
    let mut rgb = vec![128u8; width * height * 3];
    for r in 0..4 {
        for c in 0..4 {
            let off = r * width * 3 + c * 3;
            rgb[off] = 240;
            rgb[off + 1] = 32;
            rgb[off + 2] = 16;
        }
    }
    rgb
}

/// Build a 32×32 fixture: solid mid-gray everywhere. Zero hot corner.
/// When the encoder cuts from `static_with_hot_corner` to this, every
/// MB suddenly looks the same → the small-mass slots that previously
/// served the hot corner go fully unreferenced.
fn static_uniform_gray(width: usize, height: usize) -> Vec<u8> {
    vec![128u8; width * height * 3]
}

/// Build a 32×32 fixture with a vivid red-yellow gradient. When the
/// encoder cuts from gray to this, the persistence path's seed
/// codebook is stale (no slot represents red/yellow), so high-error
/// MBs accumulate quickly — the perfect environment for slot
/// reclamation to win on cumulative pixel fidelity.
fn vivid_gradient(width: usize, height: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; width * height * 3];
    for r in 0..height {
        for c in 0..width {
            let off = r * width * 3 + c * 3;
            rgb[off] = 240;
            rgb[off + 1] = ((c * 240) / width.max(1)) as u8;
            rgb[off + 2] = ((r * 80) / height.max(1)) as u8;
        }
    }
    rgb
}

/// Reclamation fires after the threshold on a content cut, and the
/// `last_frame_stats()` API reports the reclamation counts. Forces
/// `stale_slot_threshold = Some(2)` so 3 stale-prefix frames are
/// enough to trigger.
#[test]
fn reclamation_fires_after_threshold_on_content_cut() {
    let w = 32usize;
    let h = 32usize;
    let opts = EncoderOptions {
        stale_slot_threshold: Some(2),
        ..EncoderOptions::from_quality(50)
    };

    let mut enc = CinepakEncoder::new();
    // Intra: small hot corner that uses few slots.
    let _intra = enc
        .encode_intra(&static_with_hot_corner(w, h), w as u32, h as u32, opts)
        .unwrap();

    // 3 inter frames of solid gray — drops the hot-corner slots into
    // staleness limbo. After the third frame, those slots' staleness
    // counter has incremented 3 times (per-frame, not per-strip), so
    // any subsequent inter frame should pick those slots up for
    // reclamation when there's a high-residual MB.
    for _ in 0..3 {
        let _ = enc
            .encode_inter(&static_uniform_gray(w, h), w as u32, h as u32, opts)
            .unwrap();
        let st = enc.last_frame_stats();
        // Reclamation should NOT fire while the input is uniform —
        // there's no high-residual outlier worth reclaiming a slot
        // for.
        assert_eq!(
            st.reclaimed_v4_slots, 0,
            "reclamation should not fire on uniform input"
        );
    }

    // Cut to vivid content. The seed's slots that were trained on
    // hot-corner red/orange went stale during the gray run — and the
    // vivid gradient produces high-residual MBs the encoder should
    // reclaim those stale slots for.
    let _f = enc
        .encode_inter(&vivid_gradient(w, h), w as u32, h as u32, opts)
        .unwrap();
    let st = enc.last_frame_stats();
    let total_reclaimed = st.reclaimed_v4_slots + st.reclaimed_v1_slots;
    eprintln!(
        "after cut: reclaimed_v4={} reclaimed_v1={} forced_full_chunks={}",
        st.reclaimed_v4_slots, st.reclaimed_v1_slots, st.forced_full_chunks
    );
    assert!(
        total_reclaimed > 0,
        "expected reclamation to fire on content cut after threshold; got 0"
    );
    assert!(
        st.forced_full_chunks > 0,
        "expected at least one forced-full codebook chunk after reclamation; got {}",
        st.forced_full_chunks
    );
}

/// `stale_slot_threshold = None` disables reclamation entirely — even
/// after many stale-prefix frames a content cut produces zero
/// reclamations.
#[test]
fn reclamation_disabled_via_none_threshold() {
    let w = 32usize;
    let h = 32usize;
    let opts = EncoderOptions {
        stale_slot_threshold: None,
        ..EncoderOptions::from_quality(50)
    };

    let mut enc = CinepakEncoder::new();
    let _intra = enc
        .encode_intra(&static_with_hot_corner(w, h), w as u32, h as u32, opts)
        .unwrap();
    for _ in 0..6 {
        let _ = enc
            .encode_inter(&static_uniform_gray(w, h), w as u32, h as u32, opts)
            .unwrap();
    }
    let _f = enc
        .encode_inter(&vivid_gradient(w, h), w as u32, h as u32, opts)
        .unwrap();
    let st = enc.last_frame_stats();
    assert_eq!(
        st.reclaimed_v4_slots, 0,
        "reclamation should be disabled with stale_slot_threshold=None"
    );
    assert_eq!(st.reclaimed_v1_slots, 0);
    assert_eq!(st.forced_full_chunks, 0);
}

/// Reclamation produces a coherent decodable stream. Encode a
/// 6-frame sequence of (intra static → 3× gray → vivid → vivid →
/// vivid) and decode each frame; assert dimensions and bounded MAE.
#[test]
fn reclamation_chain_decodes_coherently() {
    let w = 32usize;
    let h = 32usize;
    let opts = EncoderOptions {
        stale_slot_threshold: Some(2),
        ..EncoderOptions::from_quality(50)
    };
    let mut enc = CinepakEncoder::new();
    let intra = enc
        .encode_intra(&static_with_hot_corner(w, h), w as u32, h as u32, opts)
        .unwrap();
    let mut frames: Vec<Vec<u8>> = vec![intra];
    for _ in 0..3 {
        frames.push(
            enc.encode_inter(&static_uniform_gray(w, h), w as u32, h as u32, opts)
                .unwrap(),
        );
    }
    for _ in 0..3 {
        frames.push(
            enc.encode_inter(&vivid_gradient(w, h), w as u32, h as u32, opts)
                .unwrap(),
        );
    }

    // Decode end-to-end.
    let mut dec = CinepakDecoder::new();
    for (i, f) in frames.iter().enumerate() {
        let img = dec.decode_frame(f, None).unwrap();
        assert_eq!(img.width, w as u32);
        assert_eq!(img.height, h as u32);
        eprintln!("frame {i}: bytes={}", f.len());
    }

    // Final-frame MAE vs the vivid source must stay bounded — the
    // encoder isn't expected to be lossless, but reclamation should
    // help converge the codebook on the new content within a couple
    // of frames.
    let last = dec.decode_frame(frames.last().unwrap(), None).unwrap();
    let stride = last.stride();
    let p = last.pixels();
    let src = vivid_gradient(w, h);
    let mut sum = 0f64;
    let mut n = 0f64;
    for r in 0..h {
        for c in 0..w {
            let off = r * stride + c * 3;
            let s_off = r * w * 3 + c * 3;
            for ch in 0..3 {
                sum += (p[off + ch] as f64 - src[s_off + ch] as f64).abs();
                n += 1.0;
            }
        }
    }
    let mae = sum / n.max(1.0);
    eprintln!("final-frame MAE = {mae:.2}");
    assert!(
        mae < 50.0,
        "final-frame MAE = {mae:.2} exceeds bound; reclamation chain must produce coherent pixels"
    );
}

/// `last_frame_stats()` resets at the start of each frame: an intra
/// after a reclaiming inter zeros all counters.
#[test]
fn last_frame_stats_resets_per_frame() {
    let w = 32usize;
    let h = 32usize;
    let opts = EncoderOptions {
        stale_slot_threshold: Some(1),
        ..EncoderOptions::from_quality(50)
    };
    let mut enc = CinepakEncoder::new();
    let _ = enc
        .encode_intra(&static_with_hot_corner(w, h), w as u32, h as u32, opts)
        .unwrap();
    for _ in 0..3 {
        let _ = enc
            .encode_inter(&static_uniform_gray(w, h), w as u32, h as u32, opts)
            .unwrap();
    }
    let _ = enc
        .encode_inter(&vivid_gradient(w, h), w as u32, h as u32, opts)
        .unwrap();
    // Encoder may or may not have reclaimed depending on internal
    // ordering, but the next intra MUST zero everything.
    let _ = enc
        .encode_intra(&static_with_hot_corner(w, h), w as u32, h as u32, opts)
        .unwrap();
    let st = enc.last_frame_stats();
    assert_eq!(st.reclaimed_v4_slots, 0);
    assert_eq!(st.reclaimed_v1_slots, 0);
    assert_eq!(st.forced_full_chunks, 0);
}

/// Adaptive-tolerance windowed bisection produces a decodable chain
/// whose total bytes stay within a generous bound of the target.
#[test]
fn adaptive_window_rate_control_decodes_and_targets_budget() {
    let w = 32usize;
    let h = 32usize;
    let n = 4usize;
    // Slow pan: very stable variance ⇒ adaptive tolerance shrinks
    // toward the min bound.
    let frames: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let mut rgb = vec![0u8; w * h * 3];
            for r in 0..h {
                for c in 0..w {
                    let src_c = (c + i) % w;
                    let off = r * w * 3 + c * 3;
                    rgb[off] = ((src_c * 255) / w.max(1)) as u8;
                    rgb[off + 1] = ((r * 255) / h.max(1)) as u8;
                    rgb[off + 2] = (((src_c + r) * 255) / (w + h)) as u8;
                }
            }
            rgb
        })
        .collect();

    let rc = TwoPassRateControl::new();
    let target_window = 2400usize;
    let r: Vec<RateControlledFrame> = rc
        .encode_at_target_window_bytes_adaptive(
            &frames,
            w as u32,
            h as u32,
            target_window,
            n,
            // tolerance_pct_min = 1% (tight when content is stable),
            // tolerance_pct_max = 20% (loose when variance spikes),
            // variance_scale_pct = 25% (stdev-pct above which we
            // saturate at max).
            1.0,
            20.0,
            25.0,
        )
        .unwrap();
    assert_eq!(r.len(), n);

    let mut dec = CinepakDecoder::new();
    for (i, f) in r.iter().enumerate() {
        let img = dec.decode_frame(&f.bytes, None).unwrap();
        eprintln!(
            "frame {i}: q={} bytes={} delta={}",
            f.quality,
            f.bytes.len(),
            f.byte_delta
        );
        assert_eq!(img.width, w as u32);
        assert_eq!(img.height, h as u32);
    }

    let total: usize = r.iter().map(|f| f.bytes.len()).sum();
    let drift_pct =
        (total as i64 - target_window as i64).unsigned_abs() as f32 * 100.0 / target_window as f32;
    eprintln!("adaptive total={total} target={target_window} drift_pct={drift_pct:.1}%");
    // Adaptive tolerance is at least as tight as the fixed-tolerance
    // path (the fixed test asserts 60% bound; we use 75% here
    // because variance-driven tolerance can be looser at startup).
    assert!(drift_pct < 75.0);
}

/// Adaptive-tolerance windowed bisection: empty input is a no-op.
#[test]
fn adaptive_window_rate_control_empty_input() {
    let rc = TwoPassRateControl::new();
    let r = rc
        .encode_at_target_window_bytes_adaptive(&[], 32, 32, 1000, 4, 1.0, 20.0, 25.0)
        .unwrap();
    assert!(r.is_empty());
}

/// Adaptive-tolerance windowed bisection: a generous budget hits
/// max quality (q=100) on every frame regardless of tolerance band.
#[test]
fn adaptive_window_rate_control_generous_budget_hits_high_q() {
    let w = 16usize;
    let h = 16usize;
    let n = 3usize;
    let frames: Vec<Vec<u8>> = (0..n).map(|_| vec![100u8; w * h * 3]).collect();
    let rc = TwoPassRateControl::new();
    let r = rc
        .encode_at_target_window_bytes_adaptive(
            &frames, w as u32, h as u32, 1_000_000, n, 1.0, 20.0, 25.0,
        )
        .unwrap();
    assert_eq!(r.len(), n);
    for f in &r {
        assert_eq!(f.quality, 100);
    }
}

/// Adaptive-tolerance windowed bisection: tolerance bounds equal ⇒
/// behaviour matches fixed-tolerance variant. Confirms the adaptive
/// variant doesn't pathologically diverge when the variance signal is
/// stripped out via collapsing the tolerance range.
#[test]
fn adaptive_window_collapsed_bounds_matches_fixed() {
    let w = 32usize;
    let h = 32usize;
    let n = 4usize;
    let frames: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let mut rgb = vec![0u8; w * h * 3];
            for r in 0..h {
                for c in 0..w {
                    let src_c = (c + i) % w;
                    let off = r * w * 3 + c * 3;
                    rgb[off] = ((src_c * 255) / w.max(1)) as u8;
                    rgb[off + 1] = ((r * 255) / h.max(1)) as u8;
                    rgb[off + 2] = (((src_c + r) * 255) / (w + h)) as u8;
                }
            }
            rgb
        })
        .collect();

    let rc = TwoPassRateControl::new();
    let target_window = 2400usize;
    let fixed_tol = 5.0f32;
    let fixed = rc
        .encode_at_target_window_bytes(&frames, w as u32, h as u32, target_window, n, fixed_tol)
        .unwrap();
    let adaptive = rc
        .encode_at_target_window_bytes_adaptive(
            &frames,
            w as u32,
            h as u32,
            target_window,
            n,
            fixed_tol,
            fixed_tol,
            25.0,
        )
        .unwrap();
    assert_eq!(fixed.len(), adaptive.len());
    for (f, a) in fixed.iter().zip(adaptive.iter()) {
        assert_eq!(
            f.quality, a.quality,
            "collapsed-bounds adaptive must match fixed"
        );
    }
}
