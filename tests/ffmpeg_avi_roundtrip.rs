//! Round-6 ffmpeg-emitted Cinepak-on-AVI roundtrip fixture.
//!
//! Use ffmpeg as a black-box *encoder* (not just decoder, as the
//! round-3 `ffmpeg_psnr.rs` test does) to drive a known-good Cinepak
//! AVI through our decoder. This catches decoder regressions against
//! the canonical ffmpeg encoder output without us having to ship a
//! pre-baked binary fixture in the tree.
//!
//! The test pipeline is:
//!
//! 1. Synthesise a 64×64 RGB24 source frame in memory.
//! 2. Pipe the raw frame to `ffmpeg -f rawvideo -pix_fmt rgb24 ... -c:v
//!    cinepak -f avi -` and read the AVI bytes back.
//! 3. Strip the AVI down to the embedded Cinepak frame (RIFF + LIST
//!    parse for the `00dc` chunk in `movi`).
//! 4. Decode the Cinepak frame through this crate and assert PSNR
//!    versus the synthetic source.
//!
//! The test gracefully skips (with `eprintln!` and an early return,
//! not a panic) when:
//!
//! - `ffmpeg` is not on `$PATH`.
//! - `OXIDEAV_SKIP_FFMPEG_TESTS` is set in the environment.
//! - The local ffmpeg build lacks the `cinepak` encoder
//!   (encoder-not-found stderr is detected and treated as skip).
//!
//! Wire-format reference: `docs/video/cinepak/spec/05-container-carriage.md`
//! describes Cinepak-in-AVI; `01-frame-and-strip.md` and
//! `02-codebooks.md` cover the codec frame layout.

use std::env;
use std::io::Write;
use std::process::{Command, Stdio};

use oxideav_cinepak::CinepakDecoder;

/// Synthesise a 64×64 smooth RGB24 frame.
fn synth_64x64() -> Vec<u8> {
    let w: usize = 64;
    let h: usize = 64;
    let mut rgb = vec![0u8; w * h * 3];
    for r in 0..h {
        for c in 0..w {
            let off = r * w * 3 + c * 3;
            let red = ((c * 255) / (w - 1)) as u8;
            let blue = ((r * 255) / (h - 1)) as u8;
            let green = (((r + c) * 255) / (w + h - 2)) as u8;
            rgb[off] = red;
            rgb[off + 1] = green;
            rgb[off + 2] = blue;
        }
    }
    rgb
}

/// Per-channel mean PSNR between two equal-sized RGB24 buffers.
fn psnr_rgb24(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sum_sq: f64 = 0.0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let d = x as f64 - y as f64;
        sum_sq += d * d;
    }
    if sum_sq == 0.0 {
        return f64::INFINITY;
    }
    let mse = sum_sq / a.len() as f64;
    20.0 * (255.0_f64).log10() - 10.0 * mse.log10()
}

fn ffmpeg_available() -> bool {
    if env::var_os("OXIDEAV_SKIP_FFMPEG_TESTS").is_some() {
        return false;
    }
    Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Walk an AVI byte stream and extract the first `00dc` chunk's
/// payload (the embedded video frame). Returns `None` on parse
/// failure — defensive: treat unrecognised AVI shapes as "skip".
fn extract_first_00dc(avi: &[u8]) -> Option<Vec<u8>> {
    if avi.len() < 12 || &avi[..4] != b"RIFF" {
        return None;
    }
    if &avi[8..12] != b"AVI " {
        return None;
    }
    // Search for `movi` LIST.
    let mut p = 12usize;
    while p + 8 <= avi.len() {
        let id = &avi[p..p + 4];
        let size = u32::from_le_bytes(avi[p + 4..p + 8].try_into().ok()?) as usize;
        if id == b"LIST" {
            // Inspect LIST type (next 4 bytes).
            if p + 12 > avi.len() {
                return None;
            }
            let list_type = &avi[p + 8..p + 12];
            if list_type == b"movi" {
                // Walk movi sub-chunks for a `00dc`.
                let mut q = p + 12;
                let movi_end = (p + 8 + size).min(avi.len());
                while q + 8 <= movi_end {
                    let sub_id = &avi[q..q + 4];
                    let sub_size = u32::from_le_bytes(avi[q + 4..q + 8].try_into().ok()?) as usize;
                    if sub_id == b"00dc" {
                        let start = q + 8;
                        let end = (start + sub_size).min(avi.len());
                        return Some(avi[start..end].to_vec());
                    }
                    // Pad to even length.
                    q += 8 + sub_size + (sub_size & 1);
                }
                return None;
            }
            // Other LIST — skip past its payload (with even-pad).
            p += 8 + size + (size & 1);
        } else {
            // Plain chunk — skip (with even-pad).
            p += 8 + size + (size & 1);
        }
    }
    None
}

/// Encode a 64×64 frame via ffmpeg's Cinepak encoder, decode the
/// embedded frame through this crate, assert PSNR ≥ 24 dB.
///
/// 24 dB is intentionally lenient — ffmpeg's Cinepak encoder applies
/// its own quality heuristics that we don't replicate, so the
/// reconstruction differs from the source by more than our own
/// encoder's path. The test exists to catch *decoder* regressions
/// against the ffmpeg-canonical wire format, not to benchmark.
#[test]
fn decode_ffmpeg_emitted_cinepak_avi() {
    if !ffmpeg_available() {
        eprintln!(
            "ffmpeg_avi_roundtrip: skipping (ffmpeg unavailable or OXIDEAV_SKIP_FFMPEG_TESTS set)"
        );
        return;
    }

    let width = 64u32;
    let height = 64u32;
    let rgb = synth_64x64();

    // Pipe raw RGB → ffmpeg → AVI on stdout.
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
        "64x64",
        "-r",
        "1",
        "-i",
        "-",
        "-c:v",
        "cinepak",
        "-vf",
        "format=rgb24",
        "-frames:v",
        "1",
        "-f",
        "avi",
        "-",
    ]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ffmpeg_avi_roundtrip: skip (spawn failed: {e})");
            return;
        }
    };
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        if let Err(e) = stdin.write_all(&rgb) {
            eprintln!("ffmpeg_avi_roundtrip: skip (stdin write: {e})");
            let _ = child.kill();
            return;
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("ffmpeg_avi_roundtrip: skip (wait_with_output: {e})");
            return;
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Some ffmpeg builds disable the cinepak encoder. Treat that
        // as a graceful skip rather than test failure.
        if stderr.contains("cinepak")
            && (stderr.contains("not found")
                || stderr.contains("Unknown encoder")
                || stderr.contains("not implemented"))
        {
            eprintln!("ffmpeg_avi_roundtrip: skip (cinepak encoder unavailable in this ffmpeg)");
            return;
        }
        eprintln!(
            "ffmpeg_avi_roundtrip: skip (ffmpeg failed status={:?}):\n{stderr}",
            out.status.code()
        );
        return;
    }
    let avi = out.stdout;
    if avi.is_empty() {
        eprintln!("ffmpeg_avi_roundtrip: skip (ffmpeg produced no output)");
        return;
    }

    let frame = match extract_first_00dc(&avi) {
        Some(f) if !f.is_empty() => f,
        _ => {
            eprintln!("ffmpeg_avi_roundtrip: skip (could not extract 00dc from AVI)");
            return;
        }
    };

    // Decode through our decoder.
    let mut dec = CinepakDecoder::new();
    let img = match dec.decode_frame(&frame, None) {
        Ok(i) => i,
        Err(e) => {
            panic!("decode of ffmpeg-emitted cinepak frame failed: {e}");
        }
    };
    assert_eq!(img.width, width, "ffmpeg-emitted frame width");
    assert_eq!(img.height, height, "ffmpeg-emitted frame height");

    // Pack to contiguous RGB24 for PSNR.
    let stride = img.stride();
    let mut packed = Vec::with_capacity((width * height * 3) as usize);
    for r in 0..height as usize {
        let off = r * stride;
        packed.extend_from_slice(&img.pixels()[off..off + (width as usize) * 3]);
    }
    let psnr = psnr_rgb24(&rgb, &packed);
    eprintln!("ffmpeg_avi_roundtrip: ffmpeg→our-decoder PSNR = {psnr:.2} dB");
    assert!(
        psnr >= 24.0,
        "ffmpeg-encoded → ours-decoded PSNR {psnr:.2} dB is below the 24 dB regression floor"
    );
}
