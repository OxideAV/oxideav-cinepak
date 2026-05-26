// Parallel-array index loops are idiomatic in codec / driver code; skip
// the lint (mirrors the crate root's `#![allow(clippy::needless_range_loop)]`).
#![allow(clippy::needless_range_loop)]

//! Standalone profiling driver for the Cinepak encoder and decoder.
//!
//! Round 160 (depth-mode profiling): the three Criterion harnesses
//! (`benches/{decode,encode,roundtrip}.rs`, round 126) measure
//! steady-state throughput in a sampling framework, but they're a poor
//! target for `samply` / `perf record` / `cargo flamegraph` because
//! Criterion's warm-up + sampling layers + estimator math show up in
//! the profile and bury the real codec hot paths. This example is a
//! flat measure-this-thing driver: it builds a deterministic
//! synthesised RGB / grayscale buffer once, then runs a fixed
//! iteration count of whichever path was requested with a single
//! `Instant::now()` / `elapsed()` pair around the whole loop.
//! Throughput is printed at the end so it doubles as a quick A/B
//! harness for the inner tweak-remeasure loop when Criterion's per-run
//! overhead is too coarse.
//!
//! Usage:
//!
//!     cargo run --example profile_cinepak --release -- <mode> [<iters>]
//!
//! Modes:
//!
//!     decode      — encode each scenario once outside the loop, decode
//!                   N times against the cached bytes
//!     encode      — synth pixels, encode N times (decoder cost excluded)
//!     roundtrip   — synth pixels, encode + decode every iteration
//!     stateful    — drive the full stateful encoder over a 5-frame GOP
//!                   (intra + 4 inter), measuring the picker / rate-ctrl
//!                   cost as it would appear in a real streaming use case
//!     all         — run every mode (default)
//!
//! With `samply`:
//!
//!     samply record -- ./target/release/examples/profile_cinepak encode 30
//!     samply record -- ./target/release/examples/profile_cinepak decode 100
//!
//! With `cargo flamegraph` (needs `cargo install flamegraph`):
//!
//!     cargo flamegraph --example profile_cinepak -- encode 30
//!
//! No external files are read — every input is synthesised in-driver
//! from a deterministic xorshift32 seed, matching the Criterion bench
//! harnesses so profile output and bench numbers correspond. Inputs
//! cover four cost-axis scenarios spanning the decode / encode picker
//! regimes the workspace README rows track.

use std::env;
use std::io::Write;
use std::time::Instant;

use oxideav_cinepak::{
    encode_gray8, encode_rgb24, encode_rgb24_round7, CinepakDecoder, CinepakEncoder, EncoderOptions,
};

/// xorshift32 — same constant the bench harnesses use so the profile
/// and bench inputs are byte-identical.
fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

/// Smooth gradient + small additive noise — the same pattern the
/// `benches/encode.rs` and `benches/decode.rs` harnesses use. Smooth
/// enough that V4 vs V1 selection has real signal; noisy enough that
/// the codebook trainer converges on something non-trivial.
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

/// A small temporal nudge: shift the gradient by a few pixels per
/// frame so inter encoding has both SKIP candidates (most blocks
/// unchanged) and V1/V4 differences along the leading edge — without
/// this every inter frame would compress to all-SKIP and the picker
/// cost the profile is trying to surface would never fire.
fn build_rgb_gradient_shifted(width: usize, height: usize, shift_x: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 3];
    let mut state: u32 = 0x1234_5678u32
        .wrapping_add(shift_x as u32)
        .wrapping_mul(0x9E37_79B1);
    for r in 0..height {
        for c in 0..width {
            let cc = (c + shift_x) % width.max(1);
            let base_y = ((r * 255) / height.max(1)) as u32;
            let base_x = ((cc * 255) / width.max(1)) as u32;
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

/// All four cost-axis scenarios, mirroring the Criterion benches.
struct Scenario {
    name: &'static str,
    width: u32,
    height: u32,
    quality: u8,
    /// `true` for the round-7 3-axis picker (decode profile uses the
    /// stateless `encode_rgb24`; encode profile dispatches to the
    /// round-7 picker which is the headline PSNR_Y path).
    use_round7_picker: bool,
    /// Per-pixel byte count (3 for `Rgb24`, 1 for `Gray8`) — drives
    /// the throughput print.
    bytes_per_pixel: usize,
    /// Default iteration count for the encode path. The decode path
    /// is roughly 50-100x cheaper so the driver scales this up for
    /// decode-only runs.
    encode_iters_default: u32,
}

fn scenarios() -> &'static [Scenario] {
    &[
        Scenario {
            name: "rgb24/64x64/q50/round7",
            width: 64,
            height: 64,
            quality: 50,
            use_round7_picker: true,
            bytes_per_pixel: 3,
            encode_iters_default: 40,
        },
        Scenario {
            name: "rgb24/320x240/q50/baseline",
            width: 320,
            height: 240,
            quality: 50,
            use_round7_picker: false,
            bytes_per_pixel: 3,
            encode_iters_default: 12,
        },
        Scenario {
            name: "rgb24/640x480/q70/baseline",
            width: 640,
            height: 480,
            quality: 70,
            use_round7_picker: false,
            bytes_per_pixel: 3,
            encode_iters_default: 6,
        },
        Scenario {
            name: "gray8/320x240/q50/baseline",
            width: 320,
            height: 240,
            quality: 50,
            use_round7_picker: false,
            bytes_per_pixel: 1,
            encode_iters_default: 16,
        },
    ]
}

/// One encode pass — stateless, returns the encoded byte vector. The
/// `Gray8` scenarios dispatch to `encode_gray8`; the `Rgb24` ones
/// to `encode_rgb24_round7` (3-axis picker — the headline PSNR_Y
/// path) when `use_round7_picker` is set, else `encode_rgb24` (the
/// 1-trial baseline).
fn encode_once(scen: &Scenario) -> Vec<u8> {
    let opts = EncoderOptions::from_quality(scen.quality);
    match (scen.bytes_per_pixel, scen.use_round7_picker) {
        (3, true) => {
            let rgb = build_rgb_gradient(scen.width as usize, scen.height as usize);
            encode_rgb24_round7(&rgb, scen.width, scen.height, opts).expect("encode_rgb24_round7")
        }
        (3, false) => {
            let rgb = build_rgb_gradient(scen.width as usize, scen.height as usize);
            encode_rgb24(&rgb, scen.width, scen.height, opts).expect("encode_rgb24")
        }
        (1, _) => {
            let gy = build_gray_gradient(scen.width as usize, scen.height as usize);
            encode_gray8(&gy, scen.width, scen.height, opts).expect("encode_gray8")
        }
        _ => unreachable!("scenario bytes_per_pixel must be 1 or 3"),
    }
}

/// One decode pass — fresh decoder each call so the cost of
/// `CinepakDecoder::new` (zero-init of the per-strip codebook
/// arrays) is folded into the measurement rather than amortised
/// across thousands of frames.
fn decode_once(bytes: &[u8]) -> usize {
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(bytes, None).expect("decode_frame");
    // Sum a plane length to keep the optimiser from dropping the call.
    f.planes.iter().map(|p| p.data.len()).sum()
}

fn print_throughput_line(label: &str, scen: &Scenario, iters: u32, elapsed_secs: f64, extra: &str) {
    let raw_bytes_per_iter = scen.width as usize * scen.height as usize * scen.bytes_per_pixel;
    let total_bytes = raw_bytes_per_iter * iters as usize;
    let per_iter_ms = elapsed_secs * 1000.0 / iters as f64;
    let mib_per_s = (total_bytes as f64) / elapsed_secs / (1024.0 * 1024.0);
    println!(
        "  {label:9} {name:30} iters={iters:>4} {per_iter_ms:8.3} ms/iter  {mib_per_s:8.2} MiB/s (raw){extra}",
        name = scen.name,
    );
}

fn profile_encode(iters_override: Option<u32>) {
    println!("== encode ==");
    for scen in scenarios() {
        let iters = iters_override.unwrap_or(scen.encode_iters_default);

        // One warm-up so any first-call lazy init isn't charged to
        // iteration #1's bucket.
        let _ = encode_once(scen);

        let t = Instant::now();
        let mut total_out_bytes = 0u64;
        for _ in 0..iters {
            let out = std::hint::black_box(encode_once(std::hint::black_box(scen)));
            total_out_bytes += out.len() as u64;
            std::hint::black_box(out);
        }
        let elapsed = t.elapsed().as_secs_f64();
        let compressed_bytes_per_iter = total_out_bytes / iters.max(1) as u64;
        let raw_bytes_per_iter = scen.width as usize * scen.height as usize * scen.bytes_per_pixel;
        let ratio = compressed_bytes_per_iter as f64 / raw_bytes_per_iter as f64;
        let extra = format!("  out={compressed_bytes_per_iter}B/iter ({ratio:.3} of input)");
        print_throughput_line("encode", scen, iters, elapsed, &extra);
        std::io::stdout().flush().ok();
    }
}

fn profile_decode(iters_override: Option<u32>) {
    println!("== decode ==");
    for scen in scenarios() {
        // Decode is the cheaper side — scale the iteration count up
        // when the caller didn't pick one explicitly.
        let iters = iters_override.unwrap_or(scen.encode_iters_default * 50);

        // Encode once OUTSIDE the timed region.
        let bytes = encode_once(scen);

        // Warm up: one decode pass.
        let _ = decode_once(&bytes);

        let t = Instant::now();
        let mut sink = 0usize;
        for _ in 0..iters {
            sink ^= decode_once(std::hint::black_box(&bytes));
        }
        std::hint::black_box(sink);
        let elapsed = t.elapsed().as_secs_f64();
        print_throughput_line("decode", scen, iters, elapsed, "");
        std::io::stdout().flush().ok();
    }
}

fn profile_roundtrip(iters_override: Option<u32>) {
    println!("== roundtrip ==");
    for scen in scenarios() {
        let iters = iters_override.unwrap_or(scen.encode_iters_default);

        // Warm-up.
        {
            let bytes = encode_once(scen);
            let _ = decode_once(&bytes);
        }

        let t = Instant::now();
        for _ in 0..iters {
            let bytes = std::hint::black_box(encode_once(std::hint::black_box(scen)));
            let n = decode_once(&bytes);
            std::hint::black_box(n);
        }
        let elapsed = t.elapsed().as_secs_f64();
        print_throughput_line("roundtrip", scen, iters, elapsed, "");
        std::io::stdout().flush().ok();
    }
}

/// Stateful 5-frame GOP (1 intra + 4 inter) — exercises the picker /
/// rolling-codebook / rate-control machinery that the stateless
/// `encode_*` entry points bypass. Mirrors the r155 ffmpeg cross-decode
/// fixture pattern (5 frames at 320×240).
fn profile_stateful(iters_override: Option<u32>) {
    println!("== stateful (5-frame GOP at 320x240 q50) ==");
    let iters = iters_override.unwrap_or(12);
    let width: u32 = 320;
    let height: u32 = 240;
    let opts = EncoderOptions::from_quality(50);

    // Build 5 deterministic frames once; reuse across iterations.
    let frames: Vec<Vec<u8>> = (0..5)
        .map(|i| build_rgb_gradient_shifted(width as usize, height as usize, i * 2))
        .collect();

    // Warm-up — one full sequence.
    {
        let mut enc = CinepakEncoder::new();
        let mut dec = CinepakDecoder::new();
        for f in &frames {
            let intra = enc
                .encode_intra(f, width, height, opts)
                .expect("warmup intra")
                .clone();
            // Reset between warm-up frames to keep the warm-up cheap —
            // the timed loop below exercises the real intra+inter mix.
            let _ = dec.decode_frame(&intra, None).expect("warm decode");
            enc.reset();
        }
    }

    let t = Instant::now();
    let mut total_bytes = 0u64;
    for _ in 0..iters {
        let mut enc = CinepakEncoder::new();
        let intra = enc
            .encode_intra(&frames[0], width, height, opts)
            .expect("intra");
        total_bytes += intra.len() as u64;
        std::hint::black_box(intra);
        for f in &frames[1..] {
            let inter = enc
                .encode_inter(std::hint::black_box(f), width, height, opts)
                .expect("inter");
            total_bytes += inter.len() as u64;
            std::hint::black_box(inter);
        }
    }
    let elapsed = t.elapsed().as_secs_f64();
    let per_seq_ms = elapsed * 1000.0 / iters as f64;
    let raw_per_seq = 5 * width as usize * height as usize * 3;
    let mib_per_s = (raw_per_seq * iters as usize) as f64 / elapsed / (1024.0 * 1024.0);
    let avg_seq_bytes = total_bytes / iters.max(1) as u64;
    println!(
        "  stateful  5-frame-gop                      iters={iters:>4} {per_seq_ms:8.3} ms/seq  {mib_per_s:8.2} MiB/s (raw)  out={avg_seq_bytes}B/seq"
    );
    std::io::stdout().flush().ok();
}

fn main() {
    let mut args = env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "all".to_string());
    let iters_override: Option<u32> = args.next().and_then(|s| s.parse().ok());

    println!(
        "=== oxideav-cinepak profile (mode={mode}, iters={}) ===",
        iters_override
            .map(|n| n.to_string())
            .unwrap_or_else(|| "default".to_string()),
    );
    println!();

    match mode.as_str() {
        "encode" => profile_encode(iters_override),
        "decode" => profile_decode(iters_override),
        "roundtrip" => profile_roundtrip(iters_override),
        "stateful" => profile_stateful(iters_override),
        "all" => {
            profile_encode(iters_override);
            println!();
            profile_decode(iters_override);
            println!();
            profile_roundtrip(iters_override);
            println!();
            profile_stateful(iters_override);
        }
        other => {
            eprintln!("unknown mode: {other:?}");
            eprintln!("usage: profile_cinepak [encode|decode|roundtrip|stateful|all] [<iters>]");
            std::process::exit(2);
        }
    }
}
