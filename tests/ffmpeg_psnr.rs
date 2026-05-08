//! Behavioural verification: encode a synthetic 320×240 RGB24 frame
//! with this crate's encoder, package it as a single-frame AVI, decode
//! it back through `ffmpeg` (treated as a black-box CLI oracle), and
//! assert PSNR ≥ 28 dB versus the original.
//!
//! Per `docs/video/cinepak/spec/00-scope.md` "Lossy-codec validation
//! criterion" we use closeness (PSNR) — Cinepak is a vector-quantisation
//! codec with built-in 12-bit chroma loss, so byte-exact output is not
//! achievable. The 28 dB floor exceeds the spec's "any non-zero PSNR
//! on natural content" baseline by a comfortable margin and matches
//! the typical FFmpeg-encoded Cinepak output on synthetic gradients.
//!
//! The test is skipped (with a printed message, not failure) when:
//!
//! - `ffmpeg` is not on `$PATH`.
//! - `OXIDEAV_SKIP_FFMPEG_TESTS` is set in the environment.
//!
//! These exits are diagnostic-only — they don't mark the test failed
//! because CI runners may not have ffmpeg installed; the local
//! workstation runs it routinely.

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use oxideav_cinepak::{encode_rgb24, EncoderOptions};

/// Synthesise a 320×240 RGB24 frame with smoothly-varying colours.
/// Diagonal gradient in luma + horizontal red ramp + vertical blue
/// ramp gives the encoder a non-trivial codebook population.
fn synth_320x240() -> Vec<u8> {
    let w: usize = 320;
    let h: usize = 240;
    let mut rgb = vec![0u8; w * h * 3];
    for r in 0..h {
        for c in 0..w {
            let off = r * w * 3 + c * 3;
            // Red ramp left→right, blue ramp top→bottom, green diagonal.
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

/// Build a minimal AVI container around a single Cinepak frame.
///
/// The AVI is the simplest container ffmpeg accepts for Cinepak; we
/// emit only the chunks ffmpeg requires (`RIFF`/`AVI ` + `LIST hdrl`
/// with `avih` + `LIST strl` with `strh`/`strf` + `LIST movi` with the
/// `00dc` data chunk + an `idx1` chunk).
fn wrap_in_avi(cinepak_frame: &[u8], width: u32, height: u32) -> Vec<u8> {
    // Compute sizes / offsets up front.
    let frame_len = cinepak_frame.len();
    // Pad the CVID frame to even length per AVI rules.
    let frame_pad = frame_len % 2;

    // strf: BITMAPINFOHEADER (40 bytes).
    let mut strf = Vec::new();
    strf.extend_from_slice(&40u32.to_le_bytes()); // biSize
    strf.extend_from_slice(&width.to_le_bytes()); // biWidth
    strf.extend_from_slice(&height.to_le_bytes()); // biHeight
    strf.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    strf.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    strf.extend_from_slice(b"cvid"); // biCompression
    strf.extend_from_slice(&(width * height * 3).to_le_bytes()); // biSizeImage
    strf.extend_from_slice(&0u32.to_le_bytes()); // biXPelsPerMeter
    strf.extend_from_slice(&0u32.to_le_bytes()); // biYPelsPerMeter
    strf.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    strf.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // strh: AVIStreamHeader (56 bytes).
    let mut strh = Vec::new();
    strh.extend_from_slice(b"vids"); // fccType
    strh.extend_from_slice(b"cvid"); // fccHandler
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwFlags
    strh.extend_from_slice(&0u16.to_le_bytes()); // wPriority
    strh.extend_from_slice(&0u16.to_le_bytes()); // wLanguage
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    strh.extend_from_slice(&1u32.to_le_bytes()); // dwScale
    strh.extend_from_slice(&15u32.to_le_bytes()); // dwRate (15 fps)
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwStart
    strh.extend_from_slice(&1u32.to_le_bytes()); // dwLength (1 frame)
    strh.extend_from_slice(&((frame_len + frame_pad) as u32).to_le_bytes()); // dwSuggestedBufferSize
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwQuality
    strh.extend_from_slice(&(width * height * 3).to_le_bytes()); // dwSampleSize
                                                                 // rcFrame
    strh.extend_from_slice(&0u16.to_le_bytes());
    strh.extend_from_slice(&0u16.to_le_bytes());
    strh.extend_from_slice(&(width as u16).to_le_bytes());
    strh.extend_from_slice(&(height as u16).to_le_bytes());

    // strl LIST: "strl" + strh chunk + strf chunk
    let mut strl = Vec::new();
    strl.extend_from_slice(b"strl");
    strl.extend_from_slice(b"strh");
    strl.extend_from_slice(&(strh.len() as u32).to_le_bytes());
    strl.extend_from_slice(&strh);
    strl.extend_from_slice(b"strf");
    strl.extend_from_slice(&(strf.len() as u32).to_le_bytes());
    strl.extend_from_slice(&strf);

    // avih: AVIMAINHEADER (56 bytes).
    let mut avih = Vec::new();
    avih.extend_from_slice(&(1_000_000u32 / 15).to_le_bytes()); // dwMicroSecPerFrame
    avih.extend_from_slice(&0u32.to_le_bytes()); // dwMaxBytesPerSec
    avih.extend_from_slice(&0u32.to_le_bytes()); // dwPaddingGranularity
    avih.extend_from_slice(&0x10u32.to_le_bytes()); // dwFlags AVIF_HASINDEX
    avih.extend_from_slice(&1u32.to_le_bytes()); // dwTotalFrames
    avih.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    avih.extend_from_slice(&1u32.to_le_bytes()); // dwStreams
    avih.extend_from_slice(&((frame_len + frame_pad) as u32).to_le_bytes()); // dwSuggestedBufferSize
    avih.extend_from_slice(&width.to_le_bytes()); // dwWidth
    avih.extend_from_slice(&height.to_le_bytes()); // dwHeight
    avih.extend_from_slice(&[0u8; 16]); // reserved

    // hdrl LIST: "hdrl" + avih chunk + strl LIST
    let mut hdrl = Vec::new();
    hdrl.extend_from_slice(b"hdrl");
    hdrl.extend_from_slice(b"avih");
    hdrl.extend_from_slice(&(avih.len() as u32).to_le_bytes());
    hdrl.extend_from_slice(&avih);
    hdrl.extend_from_slice(b"LIST");
    hdrl.extend_from_slice(&(strl.len() as u32).to_le_bytes());
    hdrl.extend_from_slice(&strl);

    // movi LIST: "movi" + 00dc chunk
    let mut movi_chunk = Vec::new();
    movi_chunk.extend_from_slice(b"00dc");
    movi_chunk.extend_from_slice(&(frame_len as u32).to_le_bytes());
    movi_chunk.extend_from_slice(cinepak_frame);
    if frame_pad != 0 {
        movi_chunk.push(0);
    }
    let mut movi = Vec::new();
    movi.extend_from_slice(b"movi");
    movi.extend_from_slice(&movi_chunk);

    // idx1 chunk: 1 entry.
    let mut idx1 = Vec::new();
    idx1.extend_from_slice(b"00dc");
    idx1.extend_from_slice(&0x10u32.to_le_bytes()); // AVIIF_KEYFRAME
    idx1.extend_from_slice(&4u32.to_le_bytes()); // offset of 00dc data
                                                 // start of movi data
                                                 // (relative to start of movi LIST data)
    idx1.extend_from_slice(&(frame_len as u32).to_le_bytes()); // size

    // Top-level RIFF.
    let mut riff = Vec::new();
    riff.extend_from_slice(b"AVI ");
    riff.extend_from_slice(b"LIST");
    riff.extend_from_slice(&(hdrl.len() as u32).to_le_bytes());
    riff.extend_from_slice(&hdrl);
    riff.extend_from_slice(b"LIST");
    riff.extend_from_slice(&(movi.len() as u32).to_le_bytes());
    riff.extend_from_slice(&movi);
    riff.extend_from_slice(b"idx1");
    riff.extend_from_slice(&(idx1.len() as u32).to_le_bytes());
    riff.extend_from_slice(&idx1);

    // Wrap.
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff.len() as u32).to_le_bytes());
    out.extend_from_slice(&riff);
    out
}

/// Compute per-channel mean PSNR between two equal-sized RGB24 buffers.
///
/// Returns `f64::INFINITY` when the buffers are pixel-identical (MSE=0).
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

/// Encode a single 320×240 frame with our encoder, decode via ffmpeg,
/// assert PSNR ≥ 28 dB.
#[test]
fn ffmpeg_psnr_floor_28db_320x240() {
    if !ffmpeg_available() {
        eprintln!(
            "ffmpeg_psnr_floor: skipping (ffmpeg unavailable or OXIDEAV_SKIP_FFMPEG_TESTS set)"
        );
        return;
    }

    let width = 320u32;
    let height = 240u32;
    let rgb = synth_320x240();

    // Mid-quality. q=50 gives 64-entry codebooks + 2 strips — should
    // comfortably beat 28 dB on a smooth gradient.
    let opts = EncoderOptions::from_quality(50);
    let frame = encode_rgb24(&rgb, width, height, opts).expect("encode");
    let avi = wrap_in_avi(&frame, width, height);

    // Write AVI to a temp file.
    let tmpdir = env::temp_dir();
    let avi_path: PathBuf = tmpdir.join(format!("oxideav_cinepak_psnr_{}.avi", std::process::id()));
    let raw_path: PathBuf = tmpdir.join(format!("oxideav_cinepak_psnr_{}.rgb", std::process::id()));
    {
        let mut f = fs::File::create(&avi_path).expect("create avi tmp");
        f.write_all(&avi).expect("write avi");
    }

    // Invoke ffmpeg to decode the AVI to packed RGB24.
    let out = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(&avi_path)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-frames:v", "1"])
        .arg(&raw_path)
        .output();

    let out = match out {
        Ok(o) => o,
        Err(e) => {
            let _ = fs::remove_file(&avi_path);
            panic!("ffmpeg invocation failed: {e}");
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let _ = fs::remove_file(&avi_path);
        let _ = fs::remove_file(&raw_path);
        panic!("ffmpeg failed (status={:?}):\n{stderr}", out.status.code());
    }

    let decoded = match fs::read(&raw_path) {
        Ok(b) => b,
        Err(e) => {
            let _ = fs::remove_file(&avi_path);
            let _ = fs::remove_file(&raw_path);
            panic!("failed to read ffmpeg output: {e}");
        }
    };
    let _ = fs::remove_file(&avi_path);
    let _ = fs::remove_file(&raw_path);

    let expected_size = (width * height * 3) as usize;
    assert_eq!(
        decoded.len(),
        expected_size,
        "ffmpeg decoded {} bytes, expected {expected_size}",
        decoded.len()
    );

    let psnr = psnr_rgb24(&rgb, &decoded);
    eprintln!("ffmpeg_psnr_floor_28db_320x240: psnr = {psnr:.2} dB");
    assert!(psnr >= 28.0, "PSNR {psnr:.2} dB is below the 28 dB floor");
}

/// Self-decode PSNR baseline (sanity): our encoder + our decoder
/// should also clear 28 dB. This runs even without ffmpeg.
#[test]
fn self_decode_psnr_floor_28db_320x240() {
    use oxideav_cinepak::CinepakDecoder;

    let width = 320u32;
    let height = 240u32;
    let rgb = synth_320x240();

    let opts = EncoderOptions::from_quality(50);
    let frame = encode_rgb24(&rgb, width, height, opts).expect("encode");
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&frame, None).expect("decode");
    assert_eq!(f.width, width);
    assert_eq!(f.height, height);

    // Tightly pack into a Vec<u8> for PSNR comparison.
    let stride = f.stride();
    let mut packed = Vec::with_capacity((width * height * 3) as usize);
    for r in 0..height as usize {
        let off = r * stride;
        packed.extend_from_slice(&f.pixels()[off..off + (width as usize) * 3]);
    }
    let psnr = psnr_rgb24(&rgb, &packed);
    eprintln!("self_decode_psnr_floor_28db_320x240: psnr = {psnr:.2} dB");
    assert!(
        psnr >= 28.0,
        "self-decode PSNR {psnr:.2} dB is below the 28 dB floor"
    );
}
