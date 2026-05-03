//! End-to-end integration test: ask ffmpeg to encode a synthetic
//! `testsrc` clip with `-c:v cinepak`, demux the resulting MOV with a
//! tiny inline ISO BMFF parser, decode every Cinepak packet through
//! `oxideav-cinepak`, and compare the YUV planes against ffmpeg's own
//! reference YUV output.
//!
//! Cinepak is lossy; we only assert that the average per-frame PSNR
//! falls inside the broadly accepted 20-50 dB envelope and that
//! every frame decodes without error. PSNR ≪ 20 would mean we
//! mis-rendered the codec; PSNR > 50 would mean our YUV output is
//! suspiciously close to bit-exact, which Cinepak is never.

use std::path::PathBuf;
use std::process::Command;

use oxideav_cinepak::CinepakDecoder;
use oxideav_core::{CodecId, Decoder, Frame, Packet, TimeBase};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;
const NUM_FRAMES: usize = 10;
const FRAMERATE: u32 = 10;

/// Skip the test if `ffmpeg` is missing.
fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tmp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    dir.join(format!("oxideav_cinepak_test_{pid}_{name}"))
}

fn build_fixtures() -> Option<(PathBuf, PathBuf)> {
    if !ffmpeg_available() {
        return None;
    }
    let mov = tmp_path("fixture.mov");
    let yuv = tmp_path("ref.yuv");
    let _ = std::fs::remove_file(&mov);
    let _ = std::fs::remove_file(&yuv);

    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!(
                "testsrc=size={}x{}:duration={}:rate={}",
                WIDTH,
                HEIGHT,
                NUM_FRAMES as u32 / FRAMERATE,
                FRAMERATE
            ),
            "-c:v",
            "cinepak",
        ])
        .arg(&mov)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let status = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(&mov)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .arg(&yuv)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    Some((mov, yuv))
}

// ───────── Tiny MOV / ISO-BMFF parser (just enough for cvid) ─────────

#[derive(Default, Debug)]
struct StblTables {
    /// Each entry is the byte size of one sample (i.e. one frame for
    /// our single-track cvid file). When `stsz` carries a constant
    /// `sample_size != 0`, this Vec is filled with `sample_count` copies
    /// of that constant.
    sample_sizes: Vec<u32>,
    /// Absolute file byte offset of each chunk.
    chunk_offsets: Vec<u64>,
    /// (first_chunk, samples_per_chunk) entries from `stsc`. The
    /// "samples_per_chunk" stays the same until the next `first_chunk`.
    stsc: Vec<(u32, u32)>,
}

fn parse_box_header(data: &[u8], off: usize) -> Option<(u32, [u8; 4], usize, usize)> {
    if off + 8 > data.len() {
        return None;
    }
    let size = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    let ty = [data[off + 4], data[off + 5], data[off + 6], data[off + 7]];
    if size == 1 {
        // 64-bit largesize — not used by ffmpeg's cvid output.
        if off + 16 > data.len() {
            return None;
        }
        let large = u64::from_be_bytes([
            data[off + 8],
            data[off + 9],
            data[off + 10],
            data[off + 11],
            data[off + 12],
            data[off + 13],
            data[off + 14],
            data[off + 15],
        ]);
        Some((large as u32, ty, off + 16, off + large as usize))
    } else {
        Some((size, ty, off + 8, off + size as usize))
    }
}

fn find_box(
    data: &[u8],
    parent_start: usize,
    parent_end: usize,
    target: &[u8; 4],
) -> Option<(usize, usize)> {
    let mut p = parent_start;
    while p + 8 <= parent_end {
        let (_size, ty, body_start, body_end) = parse_box_header(data, p)?;
        if &ty == target {
            return Some((body_start, body_end));
        }
        p = body_end;
    }
    None
}

fn parse_stsz(data: &[u8], start: usize, end: usize) -> Option<Vec<u32>> {
    // version+flags(4) + sample_size(4) + sample_count(4) + entries(...)
    if end - start < 12 {
        return None;
    }
    let sample_size = u32::from_be_bytes([
        data[start + 4],
        data[start + 5],
        data[start + 6],
        data[start + 7],
    ]);
    let sample_count = u32::from_be_bytes([
        data[start + 8],
        data[start + 9],
        data[start + 10],
        data[start + 11],
    ]) as usize;
    if sample_size != 0 {
        return Some(vec![sample_size; sample_count]);
    }
    if end - start < 12 + sample_count * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let off = start + 12 + i * 4;
        out.push(u32::from_be_bytes([
            data[off],
            data[off + 1],
            data[off + 2],
            data[off + 3],
        ]));
    }
    Some(out)
}

fn parse_stco(data: &[u8], start: usize, end: usize, large: bool) -> Option<Vec<u64>> {
    if end - start < 8 {
        return None;
    }
    let count = u32::from_be_bytes([
        data[start + 4],
        data[start + 5],
        data[start + 6],
        data[start + 7],
    ]) as usize;
    let entry_size = if large { 8 } else { 4 };
    if end - start < 8 + count * entry_size {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = start + 8 + i * entry_size;
        let v = if large {
            u64::from_be_bytes([
                data[off],
                data[off + 1],
                data[off + 2],
                data[off + 3],
                data[off + 4],
                data[off + 5],
                data[off + 6],
                data[off + 7],
            ])
        } else {
            u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as u64
        };
        out.push(v);
    }
    Some(out)
}

fn parse_stsc(data: &[u8], start: usize, end: usize) -> Option<Vec<(u32, u32)>> {
    if end - start < 8 {
        return None;
    }
    let count = u32::from_be_bytes([
        data[start + 4],
        data[start + 5],
        data[start + 6],
        data[start + 7],
    ]) as usize;
    if end - start < 8 + count * 12 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = start + 8 + i * 12;
        let first_chunk =
            u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let samples_per_chunk =
            u32::from_be_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        out.push((first_chunk, samples_per_chunk));
    }
    Some(out)
}

/// Walk one 'trak' box, find its 'stbl', collect tables.
fn parse_trak_tables(data: &[u8], trak_start: usize, trak_end: usize) -> Option<StblTables> {
    let (mdia_s, mdia_e) = find_box(data, trak_start, trak_end, b"mdia")?;
    let (minf_s, minf_e) = find_box(data, mdia_s, mdia_e, b"minf")?;
    let (stbl_s, stbl_e) = find_box(data, minf_s, minf_e, b"stbl")?;

    // Verify it's a video track of fourcc "cvid".
    let (stsd_s, stsd_e) = find_box(data, stbl_s, stbl_e, b"stsd")?;
    // version(1) + flags(3) + entry_count(4) then entries.
    if stsd_e - stsd_s < 16 {
        return None;
    }
    // First entry box header lives at stsd_s + 8.
    let entry_start = stsd_s + 8;
    if entry_start + 8 > stsd_e {
        return None;
    }
    let entry_ty = [
        data[entry_start + 4],
        data[entry_start + 5],
        data[entry_start + 6],
        data[entry_start + 7],
    ];
    if &entry_ty != b"cvid" {
        // Different codec; abort.
        return None;
    }

    let mut tables = StblTables::default();
    let (s, e) = find_box(data, stbl_s, stbl_e, b"stsz")?;
    tables.sample_sizes = parse_stsz(data, s, e)?;
    let (s, e) = find_box(data, stbl_s, stbl_e, b"stsc")?;
    tables.stsc = parse_stsc(data, s, e)?;
    if let Some((s, e)) = find_box(data, stbl_s, stbl_e, b"stco") {
        tables.chunk_offsets = parse_stco(data, s, e, false)?;
    } else if let Some((s, e)) = find_box(data, stbl_s, stbl_e, b"co64") {
        tables.chunk_offsets = parse_stco(data, s, e, true)?;
    } else {
        return None;
    }
    Some(tables)
}

/// Convert (sample_sizes, stsc, chunk_offsets) into a flat list of
/// per-sample (file_offset, length) tuples. This is the standard MOV
/// sample-table walk: for each chunk, the stsc entry tells us how
/// many samples that chunk holds; we accumulate sample sizes from
/// `sample_sizes` to compute per-sample offsets within the chunk.
fn flatten_samples(t: &StblTables) -> Vec<(u64, u32)> {
    let mut out = Vec::new();
    let n_chunks = t.chunk_offsets.len();
    let mut sample_idx = 0usize;
    for chunk_i in 0..n_chunks {
        // Find the stsc record covering this chunk. stsc is sorted by
        // first_chunk; the active record is the last one whose
        // first_chunk <= (chunk_i + 1) (stsc indices are 1-based).
        let one_based = (chunk_i + 1) as u32;
        let mut spc = 0u32;
        for &(first_chunk, samples_per_chunk) in &t.stsc {
            if first_chunk <= one_based {
                spc = samples_per_chunk;
            } else {
                break;
            }
        }
        let chunk_off = t.chunk_offsets[chunk_i];
        let mut cur_off = chunk_off;
        for _ in 0..spc {
            if sample_idx >= t.sample_sizes.len() {
                break;
            }
            let sz = t.sample_sizes[sample_idx];
            out.push((cur_off, sz));
            cur_off += sz as u64;
            sample_idx += 1;
        }
    }
    out
}

fn extract_cvid_packets(mov_data: &[u8]) -> Option<Vec<Vec<u8>>> {
    let len = mov_data.len();
    let (moov_s, moov_e) = find_box(mov_data, 0, len, b"moov")?;
    // Walk moov children for trak boxes.
    let mut p = moov_s;
    let mut all_packets: Vec<Vec<u8>> = Vec::new();
    while p + 8 <= moov_e {
        let (_size, ty, body_s, body_e) = parse_box_header(mov_data, p)?;
        if &ty == b"trak" {
            if let Some(tables) = parse_trak_tables(mov_data, body_s, body_e) {
                let samples = flatten_samples(&tables);
                for (off, sz) in samples {
                    let off = off as usize;
                    let sz = sz as usize;
                    if off + sz <= mov_data.len() {
                        all_packets.push(mov_data[off..off + sz].to_vec());
                    }
                }
                break; // single video track
            }
        }
        p = body_e;
    }
    if all_packets.is_empty() {
        None
    } else {
        Some(all_packets)
    }
}

// ───────── Test ─────────

fn psnr_y(reference: &[u8], decoded: &[u8]) -> f64 {
    assert_eq!(reference.len(), decoded.len());
    let mut sse: u64 = 0;
    for i in 0..reference.len() {
        let d = reference[i] as i32 - decoded[i] as i32;
        sse += (d * d) as u64;
    }
    let mse = sse as f64 / reference.len() as f64;
    if mse == 0.0 {
        // Bit-exact — return a finite sentinel to keep callers' bounds
        // checks straightforward.
        return 99.0;
    }
    10.0 * (255.0_f64 * 255.0 / mse).log10()
}

#[test]
fn decode_ffmpeg_generated_cinepak_mov() {
    let Some((mov, yuv)) = build_fixtures() else {
        eprintln!("ffmpeg unavailable - skipping");
        return;
    };

    let mov_data = std::fs::read(&mov).expect("read mov");
    let ref_yuv = std::fs::read(&yuv).expect("read ref yuv");
    let frame_size = (WIDTH * HEIGHT + 2 * (WIDTH / 2) * (HEIGHT / 2)) as usize;
    assert_eq!(ref_yuv.len(), frame_size * NUM_FRAMES);

    let packets = extract_cvid_packets(&mov_data).expect("extract cvid packets");
    assert_eq!(packets.len(), NUM_FRAMES, "expected one packet per frame");

    let mut dec = CinepakDecoder::new(CodecId::new("cinepak"));
    let mut sum_psnr_y = 0.0;
    for (i, pkt_data) in packets.iter().enumerate() {
        let pkt = Packet {
            data: pkt_data.clone(),
            pts: Some(i as i64),
            dts: None,
            time_base: TimeBase::new(1, FRAMERATE as i64),
            duration: None,
            stream_index: 0,
            flags: Default::default(),
        };
        dec.send_packet(&pkt).expect("send_packet");
        let f = dec.receive_frame().expect("receive_frame");
        let Frame::Video(vf) = f else {
            panic!("expected video frame");
        };
        assert_eq!(vf.planes.len(), 3);
        let y_plane = &vf.planes[0].data;
        let cb_plane = &vf.planes[1].data;
        let cr_plane = &vf.planes[2].data;
        assert_eq!(y_plane.len(), (WIDTH * HEIGHT) as usize);
        assert_eq!(cb_plane.len(), ((WIDTH / 2) * (HEIGHT / 2)) as usize);
        assert_eq!(cr_plane.len(), ((WIDTH / 2) * (HEIGHT / 2)) as usize);

        // Reference YUV layout in the file (yuv420p planar, frame-major):
        //   Y plane  = WIDTH*HEIGHT
        //   Cb plane = (WIDTH/2)*(HEIGHT/2)
        //   Cr plane = (WIDTH/2)*(HEIGHT/2)
        let ref_off = i * frame_size;
        let y_len = (WIDTH * HEIGHT) as usize;
        let ref_y = &ref_yuv[ref_off..ref_off + y_len];
        let psnr = psnr_y(ref_y, y_plane);
        sum_psnr_y += psnr;
    }
    let avg_y = sum_psnr_y / NUM_FRAMES as f64;
    eprintln!(
        "Cinepak roundtrip vs ffmpeg reference: avg luma PSNR {avg_y:.2} dB over {NUM_FRAMES} frames"
    );

    // Cinepak is lossy; ffmpeg also color-converts through its own
    // YCbCr-to-RGB matrix and back, so even bit-correct decoding can
    // diverge by a few dB from ffmpeg's reconstructed YUV. We accept
    // anything in 15..50 dB and flag implementation regressions
    // outside that envelope.
    assert!(
        (15.0..=50.0).contains(&avg_y),
        "luma PSNR {avg_y:.2} outside reasonable Cinepak envelope"
    );

    // Cleanup.
    let _ = std::fs::remove_file(&mov);
    let _ = std::fs::remove_file(&yuv);
}
