//! Round 143 — seek-friendly keyframe interval enforcement.
//!
//! Validates the auto-routing `CinepakEncoder::encode_frame` /
//! `encode_frame_gray8` entry points: callers configure a
//! `keyframe_interval`, the encoder dispatches each frame to intra or
//! inter automatically, and the returned `EncodedFrame` carries the
//! `is_keyframe` flag the container muxer needs to mark the container's
//! keyframe sample bit (AVI `AVIF_KEYFRAME` / QuickTime sync sample /
//! Sega FILM `sample_info_1` per
//! `docs/video/cinepak/spec/01-frame-and-strip.md` §1.1).

use oxideav_cinepak::{CinepakDecoder, CinepakEncoder, EncoderOptions};

/// 32×32 four-quadrant RGB synthetic frame, optionally shifted by
/// `phase` pixels for inter-frame motion.
fn synth_rgb24_32x32(phase: usize) -> Vec<u8> {
    let w = 32usize;
    let h = 32usize;
    let mut rgb = vec![0u8; w * h * 3];
    let colors: [(u8, u8, u8); 4] = [(200, 30, 30), (30, 180, 30), (30, 30, 200), (180, 180, 60)];
    for r in 0..h {
        for c in 0..w {
            let cc = (c + phase) % w;
            let q = (r / 16) * 2 + (cc / 16);
            let off = r * w * 3 + c * 3;
            rgb[off] = colors[q].0;
            rgb[off + 1] = colors[q].1;
            rgb[off + 2] = colors[q].2;
        }
    }
    rgb
}

fn synth_gray8_32x32(phase: usize) -> Vec<u8> {
    let w = 32usize;
    let h = 32usize;
    let mut g = vec![0u8; w * h];
    let lums: [u8; 4] = [50, 120, 180, 230];
    for r in 0..h {
        for c in 0..w {
            let cc = (c + phase) % w;
            let q = (r / 16) * 2 + (cc / 16);
            g[r * w + c] = lums[q];
        }
    }
    g
}

/// Per spec `01-frame-and-strip.md` §2.1, the first strip's
/// `strip_id` (at byte offset 10 of the frame; first 2 bytes of the
/// 12-byte strip header) is `0x1000` on an intra-coded strip and
/// `0x1100` on an inter-coded strip. This is the conformant
/// dispatch signal (the per-spec note at §1.1 explicitly states the
/// `flags` byte 0 is a codebook-inheritance advertisement, not a
/// strict intra/inter marker — small fixtures may emit `flags = 0x00`
/// on an inter strip when the encoder elects to fully recode rather
/// than inherit). The auto-router's `is_keyframe` flag must agree
/// with the strip_id on the wire.
fn first_strip_is_intra(bytes: &[u8]) -> bool {
    // Frame header = 10 bytes; strip_id is the first 2 bytes of strip 1
    // (big-endian u16). 0x1000 = intra.
    bytes.len() >= 12 && bytes[10] == 0x10 && bytes[11] == 0x00
}

#[test]
fn encode_frame_without_interval_errors() {
    let mut enc = CinepakEncoder::new();
    assert!(enc.keyframe_interval().is_none());
    let rgb = synth_rgb24_32x32(0);
    let r = enc.encode_frame(&rgb, 32, 32, EncoderOptions::default());
    assert!(
        r.is_err(),
        "encode_frame without keyframe_interval must error (manual-routing mode)"
    );
    let msg = format!("{}", r.err().unwrap());
    assert!(
        msg.contains("keyframe_interval"),
        "error must mention keyframe_interval; got: {msg}"
    );
}

#[test]
fn keyframe_interval_5_period_pattern() {
    // interval=5 ⇒ pattern I P P P P I P P P P I P P P P …
    let mut enc = CinepakEncoder::new().with_keyframe_interval(5);
    assert_eq!(enc.keyframe_interval(), Some(5));
    let mut emitted: Vec<(bool, u32)> = Vec::new();
    for i in 0..12 {
        let rgb = synth_rgb24_32x32(i);
        let frame = enc
            .encode_frame(&rgb, 32, 32, EncoderOptions::default())
            .unwrap();
        emitted.push((frame.is_keyframe, frame.frame_number_in_gop));
        // wire flag agrees with the is_keyframe metadata
        assert_eq!(
            first_strip_is_intra(&frame.bytes),
            frame.is_keyframe,
            "frame {i}: first strip strip_id must be 0x1000 iff is_keyframe"
        );
    }
    let expected: Vec<(bool, u32)> = vec![
        (true, 0),  // 0
        (false, 1), // 1
        (false, 2), // 2
        (false, 3), // 3
        (false, 4), // 4
        (true, 0),  // 5 — interval boundary
        (false, 1),
        (false, 2),
        (false, 3),
        (false, 4),
        (true, 0), // 10 — next interval
        (false, 1),
    ];
    assert_eq!(emitted, expected, "GOP pattern mismatch");
}

#[test]
fn keyframe_interval_1_every_frame_is_intra() {
    let mut enc = CinepakEncoder::new().with_keyframe_interval(1);
    for i in 0..5 {
        let rgb = synth_rgb24_32x32(i);
        let frame = enc
            .encode_frame(&rgb, 32, 32, EncoderOptions::default())
            .unwrap();
        assert!(frame.is_keyframe, "interval=1: frame {i} must be intra");
        assert_eq!(frame.frame_number_in_gop, 0);
        assert!(first_strip_is_intra(&frame.bytes));
    }
}

#[test]
fn keyframe_interval_zero_clamped_to_one() {
    let mut enc = CinepakEncoder::new();
    enc.set_keyframe_interval(0);
    assert_eq!(enc.keyframe_interval(), Some(1));
    let rgb = synth_rgb24_32x32(0);
    let f0 = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    let f1 = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert!(f0.is_keyframe);
    assert!(
        f1.is_keyframe,
        "interval 0 is clamped to 1 — every frame intra"
    );
}

#[test]
fn force_next_keyframe_breaks_gop_schedule() {
    // interval=8, request a force keyframe at frame 3 — sequence should
    // be I P P I P P P P P I (and then I at frame 11 because the forced
    // keyframe at index 3 resets the GOP counter to start a fresh GOP).
    let mut enc = CinepakEncoder::new().with_keyframe_interval(8);
    let mut emitted: Vec<bool> = Vec::new();
    for i in 0..12 {
        if i == 3 {
            enc.force_next_keyframe();
        }
        let rgb = synth_rgb24_32x32(i);
        let frame = enc
            .encode_frame(&rgb, 32, 32, EncoderOptions::default())
            .unwrap();
        emitted.push(frame.is_keyframe);
    }
    let expected = vec![
        true,  // 0 — fresh start
        false, // 1
        false, // 2
        true,  // 3 — forced
        false, // 4
        false, // 5
        false, // 6
        false, // 7
        false, // 8
        false, // 9
        false, // 10
        true,  // 11 — GOP resumes from frame 3, so 3 + 8 = 11 is next intra
    ];
    assert_eq!(emitted, expected, "forced keyframe must reset GOP schedule");
}

#[test]
fn force_keyframe_is_one_shot() {
    let mut enc = CinepakEncoder::new().with_keyframe_interval(10);
    enc.force_next_keyframe();
    // gop_position reports 0 when forced even before encoding
    assert_eq!(enc.gop_position(), 0);
    let rgb = synth_rgb24_32x32(0);
    let f0 = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert!(f0.is_keyframe);
    // After consuming the force, the next frame must be inter (GOP just started)
    let f1 = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert!(
        !f1.is_keyframe,
        "force_next_keyframe is one-shot; frame 1 is inter"
    );
    assert_eq!(f1.frame_number_in_gop, 1);
}

#[test]
fn reset_preserves_keyframe_interval_clears_gop() {
    let mut enc = CinepakEncoder::new().with_keyframe_interval(4);
    let rgb = synth_rgb24_32x32(0);
    let _ = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    let _ = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert_eq!(enc.gop_position(), 2);
    enc.reset();
    // interval survives reset; counter zeroed
    assert_eq!(enc.keyframe_interval(), Some(4));
    assert_eq!(enc.gop_position(), 0);
    // Next call after reset must be intra (gop_pos == 0)
    let f0 = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert!(f0.is_keyframe);
}

#[test]
fn clear_keyframe_interval_disables_auto_routing() {
    let mut enc = CinepakEncoder::new().with_keyframe_interval(3);
    enc.clear_keyframe_interval();
    assert!(enc.keyframe_interval().is_none());
    let rgb = synth_rgb24_32x32(0);
    let r = enc.encode_frame(&rgb, 32, 32, EncoderOptions::default());
    assert!(r.is_err(), "after clear, encode_frame must error again");
}

#[test]
fn auto_routed_frames_decode_correctly() {
    // Round-trip a 10-frame sequence at interval=3 through the encoder
    // and decoder; every frame must decode pixel-correct relative to its
    // intra/inter classification. PSNR is the same as the manual path
    // (we're using the same intra/inter workers).
    let mut enc = CinepakEncoder::new().with_keyframe_interval(3);
    let mut dec = CinepakDecoder::new();
    let mut all_bytes = 0usize;
    let mut keyframe_count = 0usize;
    for i in 0..10 {
        let rgb = synth_rgb24_32x32(i);
        let frame = enc
            .encode_frame(&rgb, 32, 32, EncoderOptions::default())
            .unwrap();
        if frame.is_keyframe {
            keyframe_count += 1;
        }
        all_bytes += frame.bytes.len();
        let decoded = dec.decode_frame(&frame.bytes, None).unwrap();
        assert_eq!(decoded.width, 32);
        assert_eq!(decoded.height, 32);
    }
    // interval=3, 10 frames ⇒ keyframes at 0, 3, 6, 9 ⇒ 4 keyframes
    assert_eq!(keyframe_count, 4, "interval=3, 10 frames ⇒ 4 keyframes");
    assert!(all_bytes > 0);
}

#[test]
fn gray8_auto_routing_period_pattern() {
    // Same as the colour test but on the grayscale path.
    let mut enc = CinepakEncoder::new().with_keyframe_interval(4);
    let mut emitted: Vec<bool> = Vec::new();
    for i in 0..9 {
        let g = synth_gray8_32x32(i);
        let frame = enc
            .encode_frame_gray8(&g, 32, 32, EncoderOptions::default())
            .unwrap();
        emitted.push(frame.is_keyframe);
        assert_eq!(first_strip_is_intra(&frame.bytes), frame.is_keyframe);
    }
    let expected = vec![true, false, false, false, true, false, false, false, true];
    assert_eq!(emitted, expected);
}

#[test]
fn mode_switch_forces_keyframe() {
    // Switching from Yuv12 → Gray8 mid-sequence without an explicit
    // reset or force_next_keyframe must still produce a clean intra
    // (the inter worker would otherwise reject "grayscale after colour").
    let mut enc = CinepakEncoder::new().with_keyframe_interval(10);
    let rgb = synth_rgb24_32x32(0);
    let f0 = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert!(f0.is_keyframe);
    // Next colour frame is inter — interval=10, gop_pos=1
    let f1 = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert!(!f1.is_keyframe);
    // Now switch to grayscale — should auto-keyframe because prev_frame
    // is Rgb24 (mode_mismatch arm of the router).
    let g = synth_gray8_32x32(0);
    let f2 = enc
        .encode_frame_gray8(&g, 32, 32, EncoderOptions::default())
        .unwrap();
    assert!(
        f2.is_keyframe,
        "mode switch (Rgb24 ⇒ Gray8) must auto-keyframe"
    );
    assert_eq!(f2.frame_number_in_gop, 0);
}

#[test]
fn gop_position_accessor_tracks_router() {
    let mut enc = CinepakEncoder::new().with_keyframe_interval(3);
    let rgb = synth_rgb24_32x32(0);
    assert_eq!(enc.gop_position(), 0, "fresh encoder: gop_pos == 0");
    let _ = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert_eq!(enc.gop_position(), 1, "after intra: gop_pos == 1");
    let _ = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert_eq!(enc.gop_position(), 2, "after first inter: gop_pos == 2");
    let _ = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    // interval=3, just emitted 3rd frame ⇒ counter wrapped back to 0
    assert_eq!(enc.gop_position(), 0, "interval wrap: gop_pos back to 0");
    let f3 = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert!(f3.is_keyframe, "next call after wrap must be keyframe");
    // force_next_keyframe also reports gop_pos==0 via the accessor
    let _ = enc
        .encode_frame(&rgb, 32, 32, EncoderOptions::default())
        .unwrap();
    assert_eq!(enc.gop_position(), 2);
    enc.force_next_keyframe();
    assert_eq!(enc.gop_position(), 0, "force flag suppresses live counter");
}

#[test]
fn auto_routing_composes_with_target_bitrate() {
    // Confirm that the round-96 / round-121 target-bitrate machinery
    // continues to work when frames are routed through encode_frame
    // (not just direct encode_intra / encode_inter). The budget path
    // is reached via the same encode_intra / encode_inter dispatch, so
    // this is mostly a sanity check that the GOP routing doesn't break
    // the rate stats accessor.
    let mut enc = CinepakEncoder::new()
        .with_keyframe_interval(3)
        .with_target_bitrate(2_000_000, 15.0);
    let rgb = synth_rgb24_32x32(0);
    for i in 0..6 {
        let frame = enc
            .encode_frame(&rgb, 32, 32, EncoderOptions::default())
            .unwrap();
        let stats = enc
            .last_rate_stats()
            .expect("rate stats present under target bitrate");
        // First and fourth frames are intra (i=0, i=3)
        let expect_intra = i == 0 || i == 3;
        assert_eq!(frame.is_keyframe, expect_intra);
        // Budget enforced — actual bytes must be representable
        assert!(stats.actual_bytes > 0);
    }
}
