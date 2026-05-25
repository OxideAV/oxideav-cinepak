//! Criterion benchmarks for the Cinepak encoder hot paths.
//!
//! Round 126 (depth-mode benchmarks): the encoder has grown a stack of
//! picker tiers between rounds 2..r9 and r47..r6/r7/r8/r9 — each adds
//! one or more trial encodes per frame. The picker tiers compose
//! roughly multiplicatively:
//!
//!   - `encode_rgb24` — 1 trial encode per call (baseline).
//!   - `encode_rgb24_best_strips` — ~3 trials (1 × |strip candidates|).
//!   - `encode_rgb24_round6` — ~9 trials (|strips| × |lambdas|).
//!   - `encode_rgb24_round7` — ~27 trials (|strips| × |lambdas| ×
//!     |luma_weights|).
//!   - `encode_rgb24_round8` — ~65 trials (per-strip independent
//!     (lambda, luma_weight)).
//!
//! These benches make those costs measurable so future "Lever N+1"
//! picker tiers can be A/B-compared against the round-9 baseline.
//!
//! Scenarios:
//!
//!   - **encode_rgb24_64x64_q50_baseline**: stateless `encode_rgb24` on
//!     the round-7 headline fixture. The baseline picker tier.
//!   - **encode_rgb24_64x64_q50_round6**: two-axis grid picker
//!     (~9 trials/frame). Cost should be ~9× the baseline.
//!   - **encode_rgb24_64x64_q50_round7**: three-axis grid picker
//!     (~27 trials/frame). The headline PSNR_Y picker.
//!   - **encode_rgb24_320x240_q50_baseline**: stateless intra encode on
//!     the round-3 mid-size fixture.
//!   - **encode_gray8_320x240_q50_round7**: grayscale RD-grid picker
//!     (round 101 / Lever N). ~9 trials (no `luma_weight` axis).
//!
//! Run with:
//!     cargo bench -p oxideav-cinepak --bench encode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_cinepak::{
    encode_gray8, encode_gray8_round7, encode_rgb24, encode_rgb24_round6, encode_rgb24_round7,
    EncoderOptions,
};

fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

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

fn bench_encode_rgb24_64x64_q50_baseline(c: &mut Criterion) {
    let rgb = build_rgb_gradient(64, 64);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("encode_rgb24_64x64_q50_baseline");
    g.throughput(Throughput::Bytes((64 * 64 * 3) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("baseline/64x64/q50"), |b| {
        b.iter(|| {
            encode_rgb24(criterion::black_box(&rgb), 64, 64, opts).expect("encode_rgb24");
        });
    });
    g.finish();
}

fn bench_encode_rgb24_64x64_q50_round6(c: &mut Criterion) {
    // Round-6 picker: 2-axis RD grid (~9 trial encodes per call). The
    // default `strip_candidates` / `lambda_candidates` are
    // `[1, 2, 4]` × `[Some(0.0), Some(2.5), opts.rdo_lambda]`.
    let rgb = build_rgb_gradient(64, 64);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("encode_rgb24_64x64_q50_round6");
    g.throughput(Throughput::Bytes((64 * 64 * 3) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("round6/64x64/q50"), |b| {
        b.iter(|| {
            encode_rgb24_round6(criterion::black_box(&rgb), 64, 64, opts).expect("round6");
        });
    });
    g.finish();
}

fn bench_encode_rgb24_64x64_q50_round7(c: &mut Criterion) {
    // Round-7 picker: 3-axis RD grid (~27 trial encodes per call), the
    // current headline picker. Sample size kept small because each
    // iteration is ~27× the baseline.
    let rgb = build_rgb_gradient(64, 64);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("encode_rgb24_64x64_q50_round7");
    g.throughput(Throughput::Bytes((64 * 64 * 3) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("round7/64x64/q50"), |b| {
        b.iter(|| {
            encode_rgb24_round7(criterion::black_box(&rgb), 64, 64, opts).expect("round7");
        });
    });
    g.finish();
}

fn bench_encode_rgb24_320x240_q50_baseline(c: &mut Criterion) {
    let rgb = build_rgb_gradient(320, 240);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("encode_rgb24_320x240_q50_baseline");
    g.throughput(Throughput::Bytes((320 * 240 * 3) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("baseline/320x240/q50"), |b| {
        b.iter(|| {
            encode_rgb24(criterion::black_box(&rgb), 320, 240, opts).expect("encode_rgb24");
        });
    });
    g.finish();
}

fn bench_encode_gray8_320x240_q50(c: &mut Criterion) {
    let gy = build_gray_gradient(320, 240);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("encode_gray8_320x240_q50");
    g.throughput(Throughput::Bytes((320 * 240) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("baseline/320x240/q50"), |b| {
        b.iter(|| {
            encode_gray8(criterion::black_box(&gy), 320, 240, opts).expect("encode_gray8");
        });
    });
    g.finish();
}

fn bench_encode_gray8_320x240_q50_round7(c: &mut Criterion) {
    // Round-101 grayscale picker: 2-axis grid (~9 trials, no
    // `luma_weight` axis — `Gray8` codebooks make `luma_weight` a no-op).
    let gy = build_gray_gradient(320, 240);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("encode_gray8_320x240_q50_round7");
    g.throughput(Throughput::Bytes((320 * 240) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("round7/320x240/q50"), |b| {
        b.iter(|| {
            encode_gray8_round7(criterion::black_box(&gy), 320, 240, opts).expect("gray round7");
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_encode_rgb24_64x64_q50_baseline,
    bench_encode_rgb24_64x64_q50_round6,
    bench_encode_rgb24_64x64_q50_round7,
    bench_encode_rgb24_320x240_q50_baseline,
    bench_encode_gray8_320x240_q50,
    bench_encode_gray8_320x240_q50_round7,
);
criterion_main!(benches);
