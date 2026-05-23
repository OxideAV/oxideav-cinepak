//! Round 104 — inter-frame grayscale encode path.
//!
//! Rounds 2..101 grew the colour (12-bit YUV / `Rgb24`) encoder a full
//! stateful inter-frame pipeline — [`CinepakEncoder::encode_intra`] /
//! [`CinepakEncoder::encode_inter`] carry a rolling V4/V1 codebook across
//! frames and emit `0x3100` SKIP / selective-update / chunk-omission wire
//! patterns — plus a stateless [`encode_rgb24_inter`] helper. The
//! grayscale path (`encode_gray8` + the round-101 picker) stayed
//! **intra-only**: there was no way to carry rolling grayscale codebooks
//! across frames.
//!
//! Round 104 adds the grayscale analogs:
//! - [`oxideav_cinepak::encode_gray8_inter`] — stateless `Gray8` inter
//!   frame against a `Gray8` `prev` reconstruction (analog of
//!   `encode_rgb24_inter`).
//! - [`oxideav_cinepak::CinepakEncoder::encode_intra_gray8`] /
//!   [`oxideav_cinepak::CinepakEncoder::encode_inter_gray8`] — stateful
//!   grayscale intra+inter with cross-frame codebook persistence and
//!   selective-update / chunk-omission (analog of `encode_intra` /
//!   `encode_inter`).
//!
//! The per-strip encoder is already mode-generic (it takes a
//! `PixelMode`), and the decoder already reconstructs `Gray8` frames and
//! copies SKIP macroblocks from a `Gray8` previous frame, so round 104 is
//! purely an encoder-API exposure of machinery already proven on the
//! colour path. Wire-format reference: spec §3.4 / §4 / §5 of
//! `docs/video/cinepak/spec/02-codebooks.md` (codebook chunk taxonomy,
//! including the `0x24xx` / `0x26xx` grayscale families) and §3 of
//! `03-vectors-and-macroblocks.md` (the `0x3100` inter vector chunk).

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{
    encode_gray8, encode_gray8_inter, CinepakDecoder, CinepakEncoder, CinepakPixelFormat,
    EncoderOptions,
};

const W: usize = 32;
const H: usize = 32;

/// Build a 32×32 four-quadrant grayscale fixture (distinct luma per
/// quadrant — gives the V4/V1 codebooks real structure to track).
fn fixture_gray() -> Vec<u8> {
    let mut gray = vec![0u8; W * H];
    let lumas = [40u8, 110, 180, 230];
    for r in 0..H {
        for c in 0..W {
            let q = (r / 16) * 2 + (c / 16);
            gray[r * W + c] = lumas[q];
        }
    }
    gray
}

/// Smooth diagonal grayscale gradient.
fn gradient_gray(w: usize, h: usize) -> Vec<u8> {
    let mut gray = vec![0u8; w * h];
    for r in 0..h {
        for c in 0..w {
            gray[r * w + c] = (((r + c) * 255) / (w + h - 2)) as u8;
        }
    }
    gray
}

/// Decode an encoded grayscale frame (via `dec`, which holds the correct
/// inter carry-over state) back to a packed `Gray8` plane.
fn decode_gray(dec: &mut CinepakDecoder, bytes: &[u8], w: usize, h: usize) -> Vec<u8> {
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

/// PSNR (dB) between two equal-length luma planes.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sse = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let d = x as f64 - y as f64;
        sse += d * d;
    }
    let mse = sse / a.len() as f64;
    if mse <= 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// Count SKIP macroblocks: a SKIP'd MB has bit-identical luma to the
/// previous reconstruction (the decoder copies prev verbatim). On a
/// structured fixture, false positives (a codebook entry coincidentally
/// re-emitting the same luma) are negligible.
fn count_skip_mbs(curr: &[u8], prev: &[u8], w: usize, h: usize) -> usize {
    let mut count = 0;
    for r in 0..(h / 4) {
        for c in 0..(w / 4) {
            let mut same = true;
            'mb: for dy in 0..4 {
                for dx in 0..4 {
                    let off = (r * 4 + dy) * w + (c * 4 + dx);
                    if curr[off] != prev[off] {
                        same = false;
                        break 'mb;
                    }
                }
            }
            if same {
                count += 1;
            }
        }
    }
    count
}

/// A stateful grayscale intra+inter sequence round-trips, and the
/// reconstructed frames stay within the codec's quantisation tolerance
/// of the source.
#[test]
fn gray_intra_inter_sequence_roundtrips() {
    let gray = fixture_gray();
    let opts = EncoderOptions::from_quality(50);

    let mut enc = CinepakEncoder::new();
    let intra = enc
        .encode_intra_gray8(&gray, W as u32, H as u32, opts)
        .unwrap();
    let mut inter_frames = Vec::new();
    for _ in 0..4 {
        inter_frames.push(
            enc.encode_inter_gray8(&gray, W as u32, H as u32, opts)
                .unwrap(),
        );
    }

    let mut dec = CinepakDecoder::new();
    let recon0 = decode_gray(&mut dec, &intra, W, H);
    // Intra reconstruction tracks the source within VQ tolerance.
    assert!(
        psnr(&recon0, &gray) >= 30.0,
        "intra PSNR too low: {:.2} dB",
        psnr(&recon0, &gray)
    );
    for f in &inter_frames {
        let recon = decode_gray(&mut dec, f, W, H);
        assert!(
            psnr(&recon, &gray) >= 30.0,
            "inter PSNR too low: {:.2} dB",
            psnr(&recon, &gray)
        );
    }
}

/// On a perfectly-static grayscale fixture, the stateful inter path
/// drives every macroblock to SKIP once the rolling codebook stabilises,
/// and the cumulative inter wire size is far smaller than the equivalent
/// stateless full-replace run.
#[test]
fn gray_static_fixture_stateful_beats_stateless_wire() {
    let gray = fixture_gray();
    let opts = EncoderOptions::from_quality(50);

    // Stateful path.
    let mut enc = CinepakEncoder::new();
    let intra = enc
        .encode_intra_gray8(&gray, W as u32, H as u32, opts)
        .unwrap();
    let mut stateful_total = 0usize;
    let mut stateful_inter: Vec<Vec<u8>> = Vec::new();
    for _ in 0..5 {
        let f = enc
            .encode_inter_gray8(&gray, W as u32, H as u32, opts)
            .unwrap();
        stateful_total += f.len();
        stateful_inter.push(f);
    }

    // Stateless path (full-replace every frame).
    let mut sl_dec = CinepakDecoder::new();
    let mut prev = sl_dec.decode_frame(&intra, None).unwrap();
    let mut stateless_total = 0usize;
    for _ in 0..5 {
        let f = encode_gray8_inter(&gray, &prev, W as u32, H as u32, opts).unwrap();
        stateless_total += f.len();
        prev = sl_dec.decode_frame(&f, None).unwrap();
    }

    assert!(
        stateful_total < stateless_total,
        "stateful inter wire ({stateful_total}B) should beat stateless ({stateless_total}B)"
    );
    let pct = (stateless_total - stateful_total) as f32 * 100.0 / stateless_total as f32;
    eprintln!(
        "gray static wire: stateful={stateful_total} stateless={stateless_total} (-{pct:.1}%)"
    );
    assert!(pct > 25.0, "expected >25% wire savings, got {pct:.1}%");

    // The last inter frame should be entirely SKIP on a static fixture.
    let mut dec = CinepakDecoder::new();
    let mut last = decode_gray(&mut dec, &intra, W, H);
    let total_mbs = (W / 4) * (H / 4);
    let mut skips = Vec::new();
    for f in &stateful_inter {
        let recon = decode_gray(&mut dec, f, W, H);
        skips.push(count_skip_mbs(&recon, &last, W, H));
        last = recon;
    }
    eprintln!("gray skip counts: {skips:?} (total mbs={total_mbs})");
    assert_eq!(
        *skips.last().unwrap(),
        total_mbs,
        "last inter frame should be all-SKIP on a static fixture: {skips:?}"
    );
}

/// The stateless `encode_gray8_inter` decodes coherently with motion
/// (changed content forces real V4/V1 updates, not just SKIP).
#[test]
fn gray_stateless_inter_with_motion_roundtrips() {
    let w = 64usize;
    let h = 64usize;
    let opts = EncoderOptions::from_quality(60);

    let frame_a = gradient_gray(w, h);
    // Frame B: shift the diagonal ramp by a large luma offset so most
    // MBs differ and must be coded (not skipped).
    let mut frame_b = vec![0u8; w * h];
    for (i, px) in frame_b.iter_mut().enumerate() {
        *px = frame_a[i].wrapping_add(60);
    }

    let intra = encode_gray8(&frame_a, w as u32, h as u32, opts).unwrap();
    let mut dec = CinepakDecoder::new();
    let prev = dec.decode_frame(&intra, None).unwrap();

    let inter = encode_gray8_inter(&frame_b, &prev, w as u32, h as u32, opts).unwrap();
    let recon = decode_gray(&mut dec, &inter, w, h);
    let q = psnr(&recon, &frame_b);
    eprintln!("gray stateless motion inter PSNR: {q:.2} dB");
    assert!(
        q >= 28.0,
        "stateless inter with motion PSNR too low: {q:.2} dB"
    );
}

/// `encode_inter_gray8` is rejected before any `encode_intra_gray8`,
/// after `reset()`, and when the prior frame was a colour frame.
#[test]
fn gray_inter_requires_grayscale_prev() {
    let gray = fixture_gray();
    let rgb = vec![128u8; W * H * 3];
    let opts = EncoderOptions::default();

    // No prior frame at all.
    let mut enc = CinepakEncoder::new();
    assert!(
        enc.encode_inter_gray8(&gray, W as u32, H as u32, opts)
            .is_err(),
        "inter before intra must fail"
    );

    // After reset, prev is cleared.
    let _ = enc
        .encode_intra_gray8(&gray, W as u32, H as u32, opts)
        .unwrap();
    enc.reset();
    assert!(
        enc.encode_inter_gray8(&gray, W as u32, H as u32, opts)
            .is_err(),
        "inter after reset must fail"
    );

    // Prior frame is colour (Rgb24) → grayscale inter must be rejected
    // (the SKIP comparison + codebook entry layout would mismatch).
    let mut enc2 = CinepakEncoder::new();
    let _ = enc2.encode_intra(&rgb, W as u32, H as u32, opts).unwrap();
    assert!(
        enc2.encode_inter_gray8(&gray, W as u32, H as u32, opts)
            .is_err(),
        "grayscale inter after a colour intra must fail"
    );
}

/// The colour stateful path is unaffected by the round-104 mode-threading
/// refactor: an RGB intra+inter pair still round-trips and the inter
/// frame is smaller than the intra on a uniform fixture.
#[test]
fn rgb_intra_inter_path_unchanged() {
    let w = 16usize;
    let h = 16usize;
    let rgb = vec![100u8; w * h * 3];
    let mut enc = CinepakEncoder::new();
    let intra = enc
        .encode_intra(&rgb, w as u32, h as u32, EncoderOptions::default())
        .unwrap();
    let inter = enc
        .encode_inter(&rgb, w as u32, h as u32, EncoderOptions::default())
        .unwrap();
    assert!(
        inter.len() < intra.len(),
        "colour inter ({}B) should stay smaller than intra ({}B)",
        inter.len(),
        intra.len()
    );
    let mut dec = CinepakDecoder::new();
    let f0 = dec.decode_frame(&intra, None).unwrap();
    let f1 = dec.decode_frame(&inter, None).unwrap();
    assert_eq!(f0.pixel_format, CinepakPixelFormat::Rgb24);
    assert_eq!(f1.pixel_format, CinepakPixelFormat::Rgb24);
}

/// `encode_gray8_inter` rejects a colour `prev` and a dimension mismatch.
#[test]
fn gray_stateless_inter_rejects_bad_prev() {
    let gray = fixture_gray();
    let rgb = vec![128u8; W * H * 3];
    let opts = EncoderOptions::default();

    // Colour prev frame.
    let rgb_intra = encode_gray8(&gray, W as u32, H as u32, opts).unwrap();
    let _ = rgb_intra; // intra here is grayscale; build a real colour prev:
    let colour_bytes = oxideav_cinepak::encode_rgb24(&rgb, W as u32, H as u32, opts).unwrap();
    let mut dec = CinepakDecoder::new();
    let colour_prev = dec.decode_frame(&colour_bytes, None).unwrap();
    assert!(
        encode_gray8_inter(&gray, &colour_prev, W as u32, H as u32, opts).is_err(),
        "Gray8 inter against a colour prev must fail"
    );

    // Dimension mismatch.
    let gray_intra = encode_gray8(&gray, W as u32, H as u32, opts).unwrap();
    let mut dec2 = CinepakDecoder::new();
    let gray_prev = dec2.decode_frame(&gray_intra, None).unwrap();
    assert!(
        encode_gray8_inter(&gray, &gray_prev, (W + 4) as u32, H as u32, opts).is_err(),
        "dimension mismatch must fail"
    );
}
