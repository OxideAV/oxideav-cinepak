//! Criterion benchmarks for the Cinepak stateful encoder + decoder
//! roundtrip — the realistic "encode a clip, decode every frame" path.
//!
//! Round 126 (depth-mode benchmarks): rounds 4..r9 grew the stateful
//! `CinepakEncoder` with rolling V4/V1 codebooks, cross-frame
//! persistence, stale-slot reclamation, and rate-control carry-over —
//! all of which produce different per-frame costs than the stateless
//! `encode_rgb24` / `encode_rgb24_inter` helpers. These benches drive
//! `CinepakEncoder::encode_intra` + a tail of `encode_inter` calls and
//! pair each with a `CinepakDecoder::decode_frame` so the full pipeline
//! is measured the way a real consumer (e.g. AVI mux + render) would.
//!
//! Scenarios (one bench per line below):
//!
//! - `roundtrip_rgb24_64x64_q50_static_4frames` — 1 intra + 4 inter on
//!   a static fixture (round-4 selective-update / chunk-omission).
//! - `roundtrip_rgb24_64x64_q50_drift_4frames` — 1 intra + 4 inter on a
//!   slow-drift fixture (round-5 cross-frame persistence under inter).
//! - `roundtrip_rgb24_320x240_q50_static_4frames` — VGA-ish 1 intra +
//!   3 inter, the largest decode-side fixture.
//! - `roundtrip_gray8_128x96_q50_static_5frames` — grayscale 1 intra +
//!   4 inter (round-104 `encode_intra_gray8` / `encode_inter_gray8`).
//!
//! Run with:
//!     cargo bench -p oxideav-cinepak --bench roundtrip

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_cinepak::{CinepakDecoder, CinepakEncoder, EncoderOptions};

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

fn drift_frame(base: &[u8], shift: u8) -> Vec<u8> {
    // Cheap per-frame drift: add `shift` to every channel with
    // saturating arithmetic. Keeps the codebook population stable
    // enough for cross-frame persistence to fire while still moving
    // enough pixels that SKIP doesn't catch most MBs.
    base.iter().map(|&x| x.saturating_add(shift)).collect()
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

fn bench_roundtrip_rgb24_64x64_static(c: &mut Criterion) {
    let rgb = build_rgb_gradient(64, 64);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("roundtrip_rgb24_64x64_q50_static_4frames");
    // Throughput = total source bytes across all 5 frames.
    g.throughput(Throughput::Bytes((64 * 64 * 3 * 5) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("rgb24/64x64/static/5f"), |b| {
        b.iter(|| {
            let mut enc = CinepakEncoder::new();
            let mut dec = CinepakDecoder::new();
            let intra = enc
                .encode_intra(criterion::black_box(&rgb), 64, 64, opts)
                .expect("intra");
            dec.decode_frame(&intra, None).expect("decode intra");
            for _ in 0..4 {
                let inter = enc
                    .encode_inter(criterion::black_box(&rgb), 64, 64, opts)
                    .expect("inter");
                dec.decode_frame(&inter, None).expect("decode inter");
            }
        });
    });
    g.finish();
}

fn bench_roundtrip_rgb24_64x64_drift(c: &mut Criterion) {
    let rgb0 = build_rgb_gradient(64, 64);
    // Pre-compute drift frames so the per-iteration cost is encode+
    // decode only, not the drift synthesis.
    let drifts: Vec<Vec<u8>> = (1..=4).map(|s| drift_frame(&rgb0, s as u8 * 4)).collect();
    let opts = EncoderOptions::from_quality(50);

    let mut g = c.benchmark_group("roundtrip_rgb24_64x64_q50_drift_4frames");
    g.throughput(Throughput::Bytes((64 * 64 * 3 * 5) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("rgb24/64x64/drift/5f"), |b| {
        b.iter(|| {
            let mut enc = CinepakEncoder::new();
            let mut dec = CinepakDecoder::new();
            let intra = enc
                .encode_intra(criterion::black_box(&rgb0), 64, 64, opts)
                .expect("intra");
            dec.decode_frame(&intra, None).expect("decode intra");
            for d in &drifts {
                let inter = enc
                    .encode_inter(criterion::black_box(d), 64, 64, opts)
                    .expect("inter");
                dec.decode_frame(&inter, None).expect("decode inter");
            }
        });
    });
    g.finish();
}

fn bench_roundtrip_rgb24_320x240_static(c: &mut Criterion) {
    let rgb = build_rgb_gradient(320, 240);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("roundtrip_rgb24_320x240_q50_static_4frames");
    g.throughput(Throughput::Bytes((320 * 240 * 3 * 4) as u64));
    g.sample_size(10);
    g.bench_function(
        BenchmarkId::from_parameter("rgb24/320x240/static/4f"),
        |b| {
            b.iter(|| {
                let mut enc = CinepakEncoder::new();
                let mut dec = CinepakDecoder::new();
                let intra = enc
                    .encode_intra(criterion::black_box(&rgb), 320, 240, opts)
                    .expect("intra");
                dec.decode_frame(&intra, None).expect("decode intra");
                for _ in 0..3 {
                    let inter = enc
                        .encode_inter(criterion::black_box(&rgb), 320, 240, opts)
                        .expect("inter");
                    dec.decode_frame(&inter, None).expect("decode inter");
                }
            });
        },
    );
    g.finish();
}

fn bench_roundtrip_gray8_128x96_static(c: &mut Criterion) {
    let gy = build_gray_gradient(128, 96);
    let opts = EncoderOptions::from_quality(50);
    let mut g = c.benchmark_group("roundtrip_gray8_128x96_q50_static_5frames");
    g.throughput(Throughput::Bytes((128 * 96 * 5) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("gray8/128x96/static/5f"), |b| {
        b.iter(|| {
            let mut enc = CinepakEncoder::new();
            let mut dec = CinepakDecoder::new();
            let intra = enc
                .encode_intra_gray8(criterion::black_box(&gy), 128, 96, opts)
                .expect("intra gray");
            dec.decode_frame(&intra, None).expect("decode intra");
            for _ in 0..4 {
                let inter = enc
                    .encode_inter_gray8(criterion::black_box(&gy), 128, 96, opts)
                    .expect("inter gray");
                dec.decode_frame(&inter, None).expect("decode inter");
            }
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_roundtrip_rgb24_64x64_static,
    bench_roundtrip_rgb24_64x64_drift,
    bench_roundtrip_rgb24_320x240_static,
    bench_roundtrip_gray8_128x96_static,
);
criterion_main!(benches);
