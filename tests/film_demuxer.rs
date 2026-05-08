//! Sega FILM container integration test.
//!
//! Exercises the full pipeline: build a synthetic FILM container
//! around a single Cinepak frame produced by the encoder, parse it
//! with [`FilmDemuxer`], extract the codec sample bytes, and decode
//! them with [`CinepakDecoder`].

use oxideav_cinepak::{encode_rgb24, probe_film, CinepakDecoder, EncoderOptions, FilmDemuxer};

/// Build a minimal 1-sample FILM container that wraps the supplied
/// Cinepak frame bytes. Layout matches `docs/video/cinepak/spec/05`
/// §2.7 (FFmpeg `film_cpk` muxer): non-deviant variant with
/// `sample_length = frame_length + 8` and 8 trailing pad bytes.
fn wrap_in_film(frame_bytes: &[u8], width: u32, height: u32) -> Vec<u8> {
    let header_length: u32 = 16 /* FILM */ + 32 /* FDSC */ + 16 /* STAB hdr */ + 16 /* 1 record */;
    let sample_length = frame_bytes.len() as u32 + 8; // §2.7 deviation
    let mut out = Vec::new();
    // FILM header.
    out.extend_from_slice(b"FILM");
    out.extend_from_slice(&header_length.to_be_bytes());
    out.extend_from_slice(b"1.09");
    out.extend_from_slice(&0u32.to_be_bytes());
    // FDSC chunk.
    out.extend_from_slice(b"FDSC");
    out.extend_from_slice(&0x20u32.to_be_bytes());
    out.extend_from_slice(b"cvid");
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.push(24);
    out.push(0);
    out.push(0);
    out.push(0);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    // STAB chunk.
    out.extend_from_slice(b"STAB");
    out.extend_from_slice(&32u32.to_be_bytes()); // chunk_length (16 + 1 record)
    out.extend_from_slice(&10u32.to_be_bytes()); // base_frequency
    out.extend_from_slice(&1u32.to_be_bytes()); // num_entries
                                                // Record: keyframe at offset 0.
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&sample_length.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // key, ts=0
    out.extend_from_slice(&1u32.to_be_bytes());
    // Sample data block: codec frame + 8 padding bytes.
    out.extend_from_slice(frame_bytes);
    out.extend_from_slice(&[0u8; 8]);
    out
}

#[test]
fn end_to_end_film_decode() {
    // Synthesize 8×8 RGB.
    let w = 8usize;
    let h = 8usize;
    let mut rgb = vec![0u8; w * h * 3];
    for r in 0..h {
        for c in 0..w {
            let off = r * w * 3 + c * 3;
            rgb[off] = (c * 32) as u8;
            rgb[off + 1] = (r * 32) as u8;
            rgb[off + 2] = ((r + c) * 16) as u8;
        }
    }
    let frame = encode_rgb24(&rgb, w as u32, h as u32, EncoderOptions::default()).unwrap();
    let film = wrap_in_film(&frame, w as u32, h as u32);

    assert!(probe_film(&film));
    let dem = FilmDemuxer::parse(&film).unwrap();
    assert_eq!(dem.fdsc.width, w as u32);
    assert_eq!(dem.fdsc.height, h as u32);
    assert!(dem.fdsc.is_standard_cinepak());
    assert_eq!(dem.samples.len(), 1);
    let s = &dem.samples[0];
    assert!(s.is_keyframe());
    let file_off = dem.sample_file_offset(0).unwrap() as usize;
    // The §2.7 deviation: sample_length includes 8 bytes of trailing pad.
    let codec_bytes = &film[file_off..file_off + frame.len()];
    let mut dec = CinepakDecoder::new();
    let decoded = dec.decode_frame(codec_bytes, Some(0)).unwrap();
    assert_eq!(decoded.width, w as u32);
    assert_eq!(decoded.height, h as u32);
}

#[test]
fn probe_rejects_non_film() {
    assert!(!probe_film(b"RIFF\x00\x00\x00\x10AVI "));
    assert!(!probe_film(b""));
}
