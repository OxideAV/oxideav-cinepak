// Pixel-coordinate arithmetic in this file uses parallel `row * stride
// + col * bpp` index forms even when one factor is `0`, because the
// uniform shape makes the layout obvious; clippy's `identity_op` /
// `erasing_op` lints fight that.
#![allow(clippy::identity_op, clippy::erasing_op)]

//! Integration tests: build synthetic Cinepak bitstreams from scratch
//! and feed them through the public decoder, asserting on the
//! reconstructed pixel buffer.
//!
//! Frame layouts exercised:
//! 1. Single-strip V1-only intra (`0x3200`).
//! 2. Single-strip V4-only intra (`0x3000` with all-`1` flag bits).
//! 3. Mixed V1+V4 single-strip intra (`0x3000` with checkerboard flag bits).
//! 4. P-frame with skip macroblocks (`0x3100`).
//! 5. Multi-strip intra frame (`0x3200` + y-sentinel rule).
//! 6. Grayscale (8-bit) single-strip intra.

use oxideav_cinepak::codebook::{
    encode_full_chunk, encode_header_only_chunk, Codebook, CodebookChunkKind, CodebookEntry,
    PixelMode, UpdateStyle, WhichCodebook, CHUNK_HEADER_SIZE,
};
use oxideav_cinepak::header::{
    FrameHeader, RawStripHeader, FRAME_HEADER_SIZE, STRIP_HEADER_SIZE, STRIP_ID_INTER,
    STRIP_ID_INTRA,
};
use oxideav_cinepak::vector::{
    encode_inter_payload, encode_mixed_intra_payload, encode_v1_only_payload, Mb,
    VECTOR_CHUNK_INTER, VECTOR_CHUNK_INTRA, VECTOR_CHUNK_V1_ONLY,
};
use oxideav_cinepak::{CinepakDecoder, CinepakPixelFormat};

// ---------------------------------------------------------------------------
// Bitstream-building helpers (test-local, not part of the public API)
// ---------------------------------------------------------------------------

fn write_chunk_header(out: &mut Vec<u8>, id: u16, payload_size: usize) {
    let chunk_size = (CHUNK_HEADER_SIZE + payload_size) as u16;
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&chunk_size.to_be_bytes());
}

fn build_strip(
    strip_id: u16,
    y_top: u16,
    x_top: u16,
    y_bottom: u16,
    x_bottom: u16,
    chunks: &[u8],
) -> Vec<u8> {
    let strip_size = (STRIP_HEADER_SIZE + chunks.len()) as u16;
    let mut out = Vec::with_capacity(strip_size as usize);
    let raw = RawStripHeader {
        strip_id,
        strip_size,
        y_top,
        x_top,
        y_bottom,
        x_bottom,
    };
    let mut hdr = [0u8; STRIP_HEADER_SIZE];
    raw.encode(&mut hdr);
    out.extend_from_slice(&hdr);
    out.extend_from_slice(chunks);
    out
}

fn build_frame(flags: u8, width: u16, height: u16, strips: &[Vec<u8>]) -> Vec<u8> {
    let strip_count = strips.len() as u16;
    let strips_size: usize = strips.iter().map(|s| s.len()).sum();
    let frame_length = (FRAME_HEADER_SIZE + strips_size) as u32;
    let mut out = Vec::with_capacity(frame_length as usize);
    let h = FrameHeader {
        flags,
        frame_length,
        width,
        height,
        strip_count,
    };
    let mut hdr = [0u8; FRAME_HEADER_SIZE];
    h.encode(&mut hdr);
    out.extend_from_slice(&hdr);
    for s in strips {
        out.extend_from_slice(s);
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Single-strip V1-only intra. 8×8 frame = 4 macroblocks. Each MB
/// references one of two V1 codebook entries: a "bright" entry and
/// a "dark" entry, in checkerboard arrangement.
#[test]
fn v1_only_intra_8x8() {
    let mut cb_v4 = Codebook::default();
    let _ = &mut cb_v4; // unused; we send a header-only V4 chunk.
    let mut cb_v1 = Codebook::default();
    cb_v1.entries[0] = CodebookEntry::from_yuv(40, 40, 40, 40, 0, 0); // gray 40
    cb_v1.entries[1] = CodebookEntry::from_yuv(200, 200, 200, 200, 0, 0); // gray 200

    let mut chunks = Vec::new();
    encode_header_only_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V4,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &mut chunks,
    );
    encode_full_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V1,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &cb_v1,
        2,
        &mut chunks,
    );
    // Vector chunk 0x3200, 4 MBs in scan order: 0, 1, 1, 0 (checkerboard).
    let mbs = vec![Mb::V1(0), Mb::V1(1), Mb::V1(1), Mb::V1(0)];
    let mut vec_payload = Vec::new();
    encode_v1_only_payload(&mbs, &mut vec_payload).unwrap();
    write_chunk_header(&mut chunks, VECTOR_CHUNK_V1_ONLY, vec_payload.len());
    chunks.extend_from_slice(&vec_payload);

    let strip = build_strip(STRIP_ID_INTRA, 0, 0, 8, 8, &chunks);
    let bytes = build_frame(0x00, 8, 8, &[strip]);

    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    assert_eq!(f.width, 8);
    assert_eq!(f.height, 8);
    assert_eq!(f.pixel_format, CinepakPixelFormat::Rgb24);
    let stride = f.stride();
    let p = f.pixels();
    // MB 0 (TL 4×4) → V1[0] → gray 40
    assert_eq!(p[0], 40);
    // MB 1 (TR 4×4) → V1[1] → gray 200
    assert_eq!(p[4 * 3], 200);
    // MB 2 (BL 4×4) → V1[1] → gray 200
    assert_eq!(p[4 * stride], 200);
    // MB 3 (BR 4×4) → V1[0] → gray 40
    assert_eq!(p[4 * stride + 4 * 3], 40);
}

/// Single-strip V4-only intra. 8×8 frame, 4 MBs all V4. Each V4 MB
/// uses four codebook entries to populate its 2×2 sub-block grid with
/// distinct luminance values.
#[test]
fn v4_only_intra_8x8() {
    let mut cb_v4 = Codebook::default();
    cb_v4.entries[0] = CodebookEntry::from_yuv(10, 20, 30, 40, 0, 0); // TL 2x2
    cb_v4.entries[1] = CodebookEntry::from_yuv(50, 60, 70, 80, 0, 0); // TR 2x2
    cb_v4.entries[2] = CodebookEntry::from_yuv(90, 100, 110, 120, 0, 0); // BL 2x2
    cb_v4.entries[3] = CodebookEntry::from_yuv(130, 140, 150, 160, 0, 0); // BR 2x2

    let mut chunks = Vec::new();
    encode_full_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V4,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &cb_v4,
        4,
        &mut chunks,
    );
    encode_header_only_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V1,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &mut chunks,
    );
    // 4 MBs all V4, each pointing at the four-quadrant entries 0..3.
    let mbs = vec![
        Mb::V4([0, 1, 2, 3]),
        Mb::V4([0, 1, 2, 3]),
        Mb::V4([0, 1, 2, 3]),
        Mb::V4([0, 1, 2, 3]),
    ];
    let mut vec_payload = Vec::new();
    encode_mixed_intra_payload(&mbs, &mut vec_payload).unwrap();
    write_chunk_header(&mut chunks, VECTOR_CHUNK_INTRA, vec_payload.len());
    chunks.extend_from_slice(&vec_payload);

    let strip = build_strip(STRIP_ID_INTRA, 0, 0, 8, 8, &chunks);
    let bytes = build_frame(0x00, 8, 8, &[strip]);

    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    let stride = f.stride();
    let p = f.pixels();
    // MB 0 (TL 4×4): r0=entry0=(10,20,30,40) covers its TL 2×2 sub-block.
    // Within that sub-block: (0,0)=10, (0,1)=20, (1,0)=30, (1,1)=40.
    assert_eq!(p[0 * stride + 0 * 3], 10);
    assert_eq!(p[0 * stride + 1 * 3], 20);
    assert_eq!(p[1 * stride + 0 * 3], 30);
    assert_eq!(p[1 * stride + 1 * 3], 40);
    // MB 0 TR 2×2 sub-block: r1=entry1=(50,60,70,80).
    assert_eq!(p[0 * stride + 2 * 3], 50);
    assert_eq!(p[0 * stride + 3 * 3], 60);
    assert_eq!(p[1 * stride + 2 * 3], 70);
    assert_eq!(p[1 * stride + 3 * 3], 80);
    // MB 0 BL 2×2 sub-block: r2=entry2=(90,100,110,120).
    assert_eq!(p[2 * stride + 0 * 3], 90);
    assert_eq!(p[3 * stride + 1 * 3], 120);
    // MB 0 BR 2×2 sub-block: r3=entry3=(130,140,150,160).
    assert_eq!(p[2 * stride + 2 * 3], 130);
    assert_eq!(p[3 * stride + 3 * 3], 160);
}

/// Mixed V1+V4 single-strip intra. 8×8 frame, four MBs in scan order:
/// V1, V4, V4, V1 (top-left and bottom-right are V1; top-right and
/// bottom-left are V4).
#[test]
fn mixed_v1_v4_intra_8x8() {
    let mut cb_v4 = Codebook::default();
    cb_v4.entries[0] = CodebookEntry::from_yuv(10, 20, 30, 40, 0, 0);
    cb_v4.entries[1] = CodebookEntry::from_yuv(50, 60, 70, 80, 0, 0);
    cb_v4.entries[2] = CodebookEntry::from_yuv(90, 100, 110, 120, 0, 0);
    cb_v4.entries[3] = CodebookEntry::from_yuv(130, 140, 150, 160, 0, 0);
    let mut cb_v1 = Codebook::default();
    cb_v1.entries[0] = CodebookEntry::from_yuv(200, 210, 220, 230, 0, 0);

    let mut chunks = Vec::new();
    encode_full_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V4,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &cb_v4,
        4,
        &mut chunks,
    );
    encode_full_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V1,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &cb_v1,
        1,
        &mut chunks,
    );
    let mbs = vec![
        Mb::V1(0),
        Mb::V4([0, 1, 2, 3]),
        Mb::V4([0, 1, 2, 3]),
        Mb::V1(0),
    ];
    let mut vec_payload = Vec::new();
    encode_mixed_intra_payload(&mbs, &mut vec_payload).unwrap();
    write_chunk_header(&mut chunks, VECTOR_CHUNK_INTRA, vec_payload.len());
    chunks.extend_from_slice(&vec_payload);

    let strip = build_strip(STRIP_ID_INTRA, 0, 0, 8, 8, &chunks);
    let bytes = build_frame(0x00, 8, 8, &[strip]);

    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    let stride = f.stride();
    let p = f.pixels();
    // MB 0 (TL) = V1[0]; quadrant 0 (TL 2×2) = Y0=200.
    assert_eq!(p[0], 200);
    // MB 1 (TR 4×4 of frame) = V4 with entry0; sub-block 0 (TL 2×2 of MB1)
    // → Y0=10. MB1 starts at col 4. So (0, 4) should be 10.
    assert_eq!(p[0 * stride + 4 * 3], 10);
    // MB 2 (BL of frame, row 4 col 0) = V4; (4, 0) = entry0 Y0 = 10.
    assert_eq!(p[4 * stride], 10);
    // MB 3 (BR of frame, row 4 col 4) = V1[0]; (4, 4) = Y0 = 200.
    assert_eq!(p[4 * stride + 4 * 3], 200);
}

/// P-frame with skip macroblocks. Frame 0 = solid V1[0]=128. Frame 1
/// is an inter strip whose vector chunk codes MB0 as V1[1]=200 and the
/// remaining 15 MBs as SKIP. Decoded frame 1 should differ from frame
/// 0 only in MB0.
#[test]
fn p_frame_with_skip() {
    // ---- Frame 0: 16×16 solid frame, V1-only intra, 16 MBs all V1[0].
    let mut cb_v4 = Codebook::default();
    let _ = &mut cb_v4;
    let mut cb_v1 = Codebook::default();
    cb_v1.entries[0] = CodebookEntry::from_yuv(128, 128, 128, 128, 0, 0);
    cb_v1.entries[1] = CodebookEntry::from_yuv(200, 200, 200, 200, 0, 0);

    let mut chunks0 = Vec::new();
    encode_header_only_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V4,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &mut chunks0,
    );
    encode_full_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V1,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &cb_v1,
        2,
        &mut chunks0,
    );
    let mbs0: Vec<Mb> = (0..16).map(|_| Mb::V1(0)).collect();
    let mut vec0 = Vec::new();
    encode_v1_only_payload(&mbs0, &mut vec0).unwrap();
    write_chunk_header(&mut chunks0, VECTOR_CHUNK_V1_ONLY, vec0.len());
    chunks0.extend_from_slice(&vec0);

    let strip0 = build_strip(STRIP_ID_INTRA, 0, 0, 16, 16, &chunks0);
    let frame0_bytes = build_frame(0x00, 16, 16, &[strip0]);

    // ---- Frame 1: 16×16 inter strip; codebook reused (header-only V4 +
    // V1), vector chunk 0x3100: MB 0 = V1(1), MBs 1..15 = Skip.
    let mut chunks1 = Vec::new();
    encode_header_only_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V4,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &mut chunks1,
    );
    encode_header_only_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V1,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &mut chunks1,
    );
    let mut mbs1 = vec![Mb::V1(1)];
    for _ in 1..16 {
        mbs1.push(Mb::Skip);
    }
    let mut vec1 = Vec::new();
    encode_inter_payload(&mbs1, &mut vec1).unwrap();
    write_chunk_header(&mut chunks1, VECTOR_CHUNK_INTER, vec1.len());
    chunks1.extend_from_slice(&vec1);

    let strip1 = build_strip(STRIP_ID_INTER, 0, 0, 16, 16, &chunks1);
    let frame1_bytes = build_frame(0x01, 16, 16, &[strip1]);

    // Decode both frames in sequence.
    let mut dec = CinepakDecoder::new();
    let f0 = dec.decode_frame(&frame0_bytes, Some(0)).unwrap();
    let f1 = dec.decode_frame(&frame1_bytes, Some(1)).unwrap();
    assert_eq!(f0.pixel_format, CinepakPixelFormat::Rgb24);
    assert_eq!(f1.pixel_format, CinepakPixelFormat::Rgb24);

    let s = f1.stride();
    let p = f1.pixels();
    // MB 0 (rows 0..3, cols 0..3) was overwritten with V1[1] = gray 200.
    for row in 0..4 {
        for col in 0..4 {
            assert_eq!(
                p[row * s + col * 3],
                200,
                "MB 0 pixel ({row},{col}) should be 200"
            );
        }
    }
    // MBs 1..15 were SKIP; they reuse frame 0's value (gray 128).
    // Pick a sample: row 0 col 4 (MB 1).
    assert_eq!(p[0 * s + 4 * 3], 128);
    // Bottom-right: row 12 col 12 (MB 15).
    assert_eq!(p[12 * s + 12 * 3], 128);
}

/// Multi-strip intra frame: two horizontal strips, each 4 rows tall,
/// same width. Tests the y-coordinate sentinel rule (`y_top=0` on the
/// second strip, `y_bottom` carries the strip height).
#[test]
fn multi_strip_intra_with_y_sentinel() {
    let mut cb_v1 = Codebook::default();
    cb_v1.entries[0] = CodebookEntry::from_yuv(50, 50, 50, 50, 0, 0); // strip 0
    cb_v1.entries[1] = CodebookEntry::from_yuv(150, 150, 150, 150, 0, 0); // strip 1

    fn build_strip_chunks(v1_idx: u8, mb_count: usize, cb: &Codebook) -> Vec<u8> {
        let mut chunks = Vec::new();
        encode_header_only_chunk(
            CodebookChunkKind {
                which: WhichCodebook::V4,
                style: UpdateStyle::Full,
                mode: PixelMode::Yuv12,
            },
            &mut chunks,
        );
        encode_full_chunk(
            CodebookChunkKind {
                which: WhichCodebook::V1,
                style: UpdateStyle::Full,
                mode: PixelMode::Yuv12,
            },
            cb,
            2,
            &mut chunks,
        );
        let mbs: Vec<Mb> = (0..mb_count).map(|_| Mb::V1(v1_idx)).collect();
        let mut vec_p = Vec::new();
        encode_v1_only_payload(&mbs, &mut vec_p).unwrap();
        write_chunk_header(&mut chunks, VECTOR_CHUNK_V1_ONLY, vec_p.len());
        chunks.extend_from_slice(&vec_p);
        chunks
    }

    // 8 cols / 4 rows / 4 = 2 MB cols × 1 MB row = 2 MBs per strip.
    let s0_chunks = build_strip_chunks(0, 2, &cb_v1);
    let s1_chunks = build_strip_chunks(1, 2, &cb_v1);

    // Strip 0: literal coords (0..4 rows).
    let s0 = build_strip(STRIP_ID_INTRA, 0, 0, 4, 8, &s0_chunks);
    // Strip 1: y-sentinel — y_top=0, y_bottom=4 (height).
    let s1 = build_strip(STRIP_ID_INTRA, 0, 0, 4, 8, &s1_chunks);

    let bytes = build_frame(0x00, 8, 8, &[s0, s1]);
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    let s = f.stride();
    let p = f.pixels();
    // Top half (rows 0..3): gray 50.
    for row in 0..4 {
        for col in 0..8 {
            assert_eq!(p[row * s + col * 3], 50, "row {row} col {col}");
        }
    }
    // Bottom half (rows 4..7): gray 150.
    for row in 4..8 {
        for col in 0..8 {
            assert_eq!(p[row * s + col * 3], 150, "row {row} col {col}");
        }
    }
}

/// 8-bit grayscale single-strip intra frame.
#[test]
fn grayscale_v1_only_intra_8x8() {
    let mut cb_v1 = Codebook::default();
    cb_v1.entries[0] = CodebookEntry::from_y(20, 60, 100, 140);

    let mut chunks = Vec::new();
    encode_header_only_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V4,
            style: UpdateStyle::Full,
            mode: PixelMode::Gray8,
        },
        &mut chunks,
    );
    encode_full_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V1,
            style: UpdateStyle::Full,
            mode: PixelMode::Gray8,
        },
        &cb_v1,
        1,
        &mut chunks,
    );
    let mbs: Vec<Mb> = (0..4).map(|_| Mb::V1(0)).collect();
    let mut vec_p = Vec::new();
    encode_v1_only_payload(&mbs, &mut vec_p).unwrap();
    write_chunk_header(&mut chunks, VECTOR_CHUNK_V1_ONLY, vec_p.len());
    chunks.extend_from_slice(&vec_p);

    let strip = build_strip(STRIP_ID_INTRA, 0, 0, 8, 8, &chunks);
    let bytes = build_frame(0x00, 8, 8, &[strip]);

    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    assert_eq!(f.pixel_format, CinepakPixelFormat::Gray8);
    assert_eq!(f.stride(), 8); // 1 byte per pixel
    let s = f.stride();
    let p = f.pixels();
    // Y0=20 covers TL 2×2 of each MB.
    assert_eq!(p[0], 20);
    assert_eq!(p[s + 1], 20);
    // Y1=60 covers TR 2×2 of MB.
    assert_eq!(p[2], 60);
    // Y2=100 covers BL 2×2 of MB.
    assert_eq!(p[2 * s], 100);
    // Y3=140 covers BR 2×2 of MB.
    assert_eq!(p[2 * s + 2], 140);
}

/// Reject malformed frame: width not a multiple of 4.
#[test]
fn rejects_non_4_aligned_dims() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0, 0, 0, 0xff, 0, 5, 0, 4, 0, 1]);
    let mut dec = CinepakDecoder::new();
    assert!(dec.decode_frame(&bytes, None).is_err());
}

/// Reject inter (`0x3100`) chunk on first frame (no previous frame to
/// reuse for skip macroblocks). The decoder must surface an error when
/// it actually encounters a Skip MB; intra strips never produce them.
#[test]
fn rejects_skip_on_first_frame() {
    let mut chunks = Vec::new();
    encode_header_only_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V4,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &mut chunks,
    );
    encode_header_only_chunk(
        CodebookChunkKind {
            which: WhichCodebook::V1,
            style: UpdateStyle::Full,
            mode: PixelMode::Yuv12,
        },
        &mut chunks,
    );
    let mbs = vec![Mb::Skip; 4];
    let mut vec_p = Vec::new();
    encode_inter_payload(&mbs, &mut vec_p).unwrap();
    write_chunk_header(&mut chunks, VECTOR_CHUNK_INTER, vec_p.len());
    chunks.extend_from_slice(&vec_p);

    let strip = build_strip(STRIP_ID_INTER, 0, 0, 8, 8, &chunks);
    let bytes = build_frame(0x01, 8, 8, &[strip]);

    let mut dec = CinepakDecoder::new();
    assert!(dec.decode_frame(&bytes, None).is_err());
}
