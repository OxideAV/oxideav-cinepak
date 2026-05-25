//! Criterion benchmarks for the Cinepak decoder hot paths.
//!
//! Round 126 (depth-mode benchmarks): cinepak has hit the per-codec
//! saturation point (decoder ≈ 96 %, encoder ≈ 98 %) so per the
//! workspace "saturated → fuzz/bench/profile" memo this round wires up
//! `criterion` benches to let future optimisation rounds A/B-test their
//! changes. This file covers the **decoder**; sibling files cover
//! `encode` (the picker-heavy encoder paths) and `roundtrip` (full
//! intra+inter sequences against a stateful encoder).
//!
//! Each scenario is self-contained: the bench encodes a fresh frame on
//! the fly with the stateless `encode_rgb24` / `encode_gray8` paths,
//! then iterates `CinepakDecoder::decode_frame` on the encoded bytes.
//! No `docs/` fixtures or external files are read.
//!
//!   - **decode_rgb24_320x240_q50**: 320×240 RGB at `q=50` — the
//!     "natural-image" baseline matching the round-3..r9 PSNR fixtures.
//!     Exercises mixed V1/V4 intra (`0x3000`) with multi-strip output.
//!   - **decode_rgb24_64x64_q50**: 64×64 RGB at `q=50` — the small-frame
//!     headline fixture (round-7 picker reports 45.21 dB PSNR_Y on this
//!     one). Bench measures per-frame fixed overhead rather than
//!     throughput.
//!   - **decode_rgb24_640x480_q70**: 640×480 RGB at `q=70` — a larger
//!     "VGA" decode covering the multi-strip path with a richer
//!     codebook (`v4_entries = 168`, `strip_count = 3`).
//!   - **decode_gray8_320x240_q50**: 320×240 grayscale at `q=50` —
//!     covers the `0x2400` / `0x2600` codebook-chunk family and the
//!     `0x3000` vector chunk with 4-byte codebook entries.
//!   - **decode_inter_skip_320x240**: 320×240 inter frame with all-SKIP
//!     macroblocks (`0x3100` chunk) — the cheapest inter decode,
//!     dominated by the chunk walker rather than the codebook lookup.
//!
//! Run with:
//!     cargo bench -p oxideav-cinepak --bench decode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_cinepak::{
    encode_gray8, encode_rgb24, encode_rgb24_inter, CinepakDecoder, EncoderOptions,
};

/// Cheap deterministic xorshift32 — synthesises "natural-ish" per-pixel
/// values for the bench inputs so codebooks have meaningful structure
/// (a pure-zero fixture would short-circuit through chunk omission and
/// hide most of the decode cost).
fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

/// Synthetic RGB gradient + small additive noise — the same pattern the
/// round-3..r9 PSNR fixtures use. Mostly-smooth so V4 vs V1 has real
/// signal, with enough per-pixel variation that the codebook trainer
/// has something to converge to.
fn build_rgb_gradient(width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 3];
    let mut state: u32 = 0x1234_5678;
    for r in 0..height {
        for c in 0..width {
            let base_y = ((r * 255) / height.max(1)) as u32;
            let base_x = ((c * 255) / width.max(1)) as u32;
            let r_v = ((base_x + base_y) / 2).min(255) as u8;
            let g_v = base_y.min(255) as u8;
            let b_v = base_x.min(255) as u8;
            let noise = xorshift_byte(&mut state) & 0x07;
            let idx = (r * width + c) * 3;
            out[idx] = r_v.wrapping_add(noise);
            out[idx + 1] = g_v.wrapping_add(noise);
            out[idx + 2] = b_v.wrapping_add(noise);
        }
    }
    out
}

fn build_gray_gradient(width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    let mut state: u32 = 0x9abc_def0;
    for r in 0..height {
        for c in 0..width {
            let base = ((r * 255) / height.max(1) + (c * 255) / width.max(1)) / 2;
            let noise = xorshift_byte(&mut state) & 0x07;
            out[r * width + c] = (base as u8).wrapping_add(noise);
        }
    }
    out
}

fn encode_rgb_frame(width: u32, height: u32, q: u8) -> Vec<u8> {
    let rgb = build_rgb_gradient(width as usize, height as usize);
    encode_rgb24(&rgb, width, height, EncoderOptions::from_quality(q)).expect("encode rgb")
}

fn encode_gray_frame(width: u32, height: u32, q: u8) -> Vec<u8> {
    let g = build_gray_gradient(width as usize, height as usize);
    encode_gray8(&g, width, height, EncoderOptions::from_quality(q)).expect("encode gray")
}

fn bench_decode_rgb24_320x240_q50(c: &mut Criterion) {
    let frame = encode_rgb_frame(320, 240, 50);
    let mut g = c.benchmark_group("decode_rgb24_320x240_q50");
    g.throughput(Throughput::Bytes((320 * 240 * 3) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgb24/320x240/q50"), |b| {
        b.iter(|| {
            let mut dec = CinepakDecoder::new();
            dec.decode_frame(criterion::black_box(&frame), None)
                .expect("decode")
        });
    });
    g.finish();
}

fn bench_decode_rgb24_64x64_q50(c: &mut Criterion) {
    let frame = encode_rgb_frame(64, 64, 50);
    let mut g = c.benchmark_group("decode_rgb24_64x64_q50");
    g.throughput(Throughput::Bytes((64 * 64 * 3) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgb24/64x64/q50"), |b| {
        b.iter(|| {
            let mut dec = CinepakDecoder::new();
            dec.decode_frame(criterion::black_box(&frame), None)
                .expect("decode")
        });
    });
    g.finish();
}

fn bench_decode_rgb24_640x480_q70(c: &mut Criterion) {
    let frame = encode_rgb_frame(640, 480, 70);
    let mut g = c.benchmark_group("decode_rgb24_640x480_q70");
    g.throughput(Throughput::Bytes((640 * 480 * 3) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgb24/640x480/q70"), |b| {
        b.iter(|| {
            let mut dec = CinepakDecoder::new();
            dec.decode_frame(criterion::black_box(&frame), None)
                .expect("decode")
        });
    });
    g.finish();
}

fn bench_decode_gray8_320x240_q50(c: &mut Criterion) {
    let frame = encode_gray_frame(320, 240, 50);
    let mut g = c.benchmark_group("decode_gray8_320x240_q50");
    g.throughput(Throughput::Bytes((320 * 240) as u64));
    g.bench_function(BenchmarkId::from_parameter("gray8/320x240/q50"), |b| {
        b.iter(|| {
            let mut dec = CinepakDecoder::new();
            dec.decode_frame(criterion::black_box(&frame), None)
                .expect("decode")
        });
    });
    g.finish();
}

fn bench_decode_inter_skip_320x240(c: &mut Criterion) {
    // Intra frame, then an inter "no change" frame: the SKIP-threshold
    // is generous enough on a static fixture that every MB skips,
    // producing a tiny `0x3100` chunk dominated by the flag-word walk.
    // `encode_rgb24_inter` takes the reconstructed `CinepakFrame` as
    // `prev`, so we hand it the decode of the intra directly.
    let rgb = build_rgb_gradient(320, 240);
    let opts = EncoderOptions::from_quality(50);
    let intra = encode_rgb24(&rgb, 320, 240, opts).expect("intra");
    let mut dec0 = CinepakDecoder::new();
    let prev_frame = dec0.decode_frame(&intra, None).expect("decode intra");
    let inter =
        encode_rgb24_inter(&rgb, &prev_frame, 320, 240, opts).expect("inter (all skip on static)");

    let mut g = c.benchmark_group("decode_inter_skip_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 3) as u64));
    g.bench_function(
        BenchmarkId::from_parameter("rgb24/320x240/inter-allskip"),
        |b| {
            b.iter(|| {
                let mut dec = CinepakDecoder::new();
                // Prime with the intra so the inter has a prev frame.
                dec.decode_frame(criterion::black_box(&intra), None)
                    .expect("decode intra");
                dec.decode_frame(criterion::black_box(&inter), None)
                    .expect("decode inter")
            });
        },
    );
    g.finish();
}

criterion_group!(
    benches,
    bench_decode_rgb24_320x240_q50,
    bench_decode_rgb24_64x64_q50,
    bench_decode_rgb24_640x480_q70,
    bench_decode_gray8_320x240_q50,
    bench_decode_inter_skip_320x240,
);
criterion_main!(benches);
