//! Sega Saturn / Sega CD / Lemmings 3DO deviant Cinepak (round-93).
//!
//! Wire-format reference:
//! `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 125–143
//! (Saturn `'cvid'` deviation) and line 189 (Lemmings 3DO 6-byte
//! prefix). All three deviations are exercised here:
//!
//! 1. **Extra header bytes** — the standard 10-byte frame header is
//!    padded with 2 trailing bytes (Saturn) or 6 trailing bytes
//!    (Lemmings 3DO) before the first strip header.
//! 2. **`frame_length` short by 8** — the codec header reports a
//!    length that is 8 bytes shy of the real frame body length. The
//!    real length comes from the FILM STAB sample record.
//! 3. **Codebook trailing pad** — codebook chunks may declare a
//!    payload size that is not a clean multiple of the entry stride.
//!    The decoder truncates to `floor(payload_len / entry_size)`
//!    entries and skips the remainder.
//!
//! All tests are synthesised — no Sega Saturn fixture is available in
//! the corpus. The synthesis follows the spec's wire description
//! exactly so a real Saturn fixture, when acquired, would decode the
//! same way.

use oxideav_cinepak::{CinepakDecoder, CinepakPixelFormat, CinepakVariant, DeviantConfig};

/// Build a single-strip 4×4 deviant Cinepak frame whose V1 codebook
/// has 255 valid entries + 2 trailing pad bytes (the Saturn 0x5FC
/// payload anomaly), and whose frame body is 8 bytes longer than the
/// `frame_length` field declares.
///
/// Returns `(deviant_bytes, expected_y0)` where `expected_y0` is the
/// luminance value the single macroblock should render with at row 0
/// col 0 (Y0 of V1 entry 0).
fn build_deviant_2_extra(extra_header_bytes: usize) -> (Vec<u8>, u8) {
    let mut bytes = Vec::new();

    // 10-byte standard frame header.
    bytes.push(0x00); // flags
    bytes.extend_from_slice(&[0x00, 0x00, 0x00]); // frame_length (patched later)
    bytes.extend_from_slice(&4u16.to_be_bytes()); // width
    bytes.extend_from_slice(&4u16.to_be_bytes()); // height
    bytes.extend_from_slice(&1u16.to_be_bytes()); // strip_count

    // Saturn deviation: 2 extra header bytes (or 6 for Lemmings 3DO).
    bytes.resize(bytes.len() + extra_header_bytes, 0x00);

    // Strip header: 12 bytes.
    let strip_hdr_pos = bytes.len();
    bytes.extend_from_slice(&0x1000u16.to_be_bytes()); // strip_id = intra
    bytes.extend_from_slice(&0u16.to_be_bytes()); // strip_size (patched)
    bytes.extend_from_slice(&0u16.to_be_bytes()); // y_top
    bytes.extend_from_slice(&0u16.to_be_bytes()); // x_top
    bytes.extend_from_slice(&4u16.to_be_bytes()); // y_bottom
    bytes.extend_from_slice(&4u16.to_be_bytes()); // x_bottom

    // V4 codebook chunk (header-only — no payload).
    bytes.extend_from_slice(&[0x20, 0x00, 0x00, 0x04]);

    // V1 codebook chunk with **255 entries + 2 trailing pad bytes**
    // (0x5FC-byte payload — exactly the 0x2000 anomaly described in
    // `Sega_FILM.wiki` line 143). chunk_size = 4 + 0x5FC = 0x600.
    bytes.extend_from_slice(&0x2200u16.to_be_bytes()); // V1 12-bit YUV full
    bytes.extend_from_slice(&0x0600u16.to_be_bytes()); // chunk_size
    for i in 0..255u16 {
        let y = (i & 0xff) as u8;
        // Entry 0: Y0=50, Y1=100, Y2=150, Y3=200, U=0, V=0.
        if i == 0 {
            bytes.extend_from_slice(&[50, 100, 150, 200, 0, 0]);
        } else {
            bytes.extend_from_slice(&[y, y, y, y, 0, 0]);
        }
    }
    bytes.extend_from_slice(&[0xAB, 0xCD]); // 2 trailing pad bytes

    // Vector chunk 0x3200 (V1-only intra) with one MB referencing V1 entry 0.
    bytes.extend_from_slice(&0x3200u16.to_be_bytes());
    bytes.extend_from_slice(&0x0005u16.to_be_bytes()); // chunk_size = 4 + 1
    bytes.push(0x00); // V1 index 0

    // Patch strip_size: total bytes in the strip including the 12-byte
    // strip header.
    let strip_size = (bytes.len() - strip_hdr_pos) as u16;
    bytes[strip_hdr_pos + 2..strip_hdr_pos + 4].copy_from_slice(&strip_size.to_be_bytes());

    // Patch frame_length: real length is `bytes.len()`, deviant header
    // declares 8 bytes LESS.
    let real_len = bytes.len() as u32;
    let declared = real_len - 8;
    bytes[1] = ((declared >> 16) & 0xff) as u8;
    bytes[2] = ((declared >> 8) & 0xff) as u8;
    bytes[3] = (declared & 0xff) as u8;

    (bytes, 50)
}

#[test]
fn decodes_deviant_saturn_frame_with_all_three_deviations() {
    let (frame_bytes, expected_y0) = build_deviant_2_extra(2);

    // The standard path must reject — the strip header would land 2
    // bytes off, the codebook payload would fail divisibility, and
    // `frame_length` undercounts by 8.
    let mut std_dec = CinepakDecoder::new();
    assert!(
        std_dec.decode_frame(&frame_bytes, None).is_err(),
        "standard decoder must reject a Saturn-deviant frame"
    );

    // The deviant path must accept and produce a valid 4×4 RGB frame.
    let mut dev_dec = CinepakDecoder::new();
    let frame = dev_dec
        .decode_deviant_frame(&frame_bytes, None, DeviantConfig::saturn())
        .expect("deviant decode must succeed");
    assert_eq!(frame.width, 4);
    assert_eq!(frame.height, 4);
    assert_eq!(frame.pixel_format, CinepakPixelFormat::Rgb24);
    let p = frame.pixels();
    // V1 quadrant: Y0 covers (0,0)–(1,1). U=V=0 so RGB = (Y, Y, Y).
    assert_eq!(p[0], expected_y0);
    assert_eq!(p[1], expected_y0);
    assert_eq!(p[2], expected_y0);
}

#[test]
fn decodes_deviant_lemmings_3do_frame_with_6_extra_bytes() {
    let (frame_bytes, expected_y0) = build_deviant_2_extra(6);

    // Saturn config rejects (wrong header offset).
    let mut dec_sat = CinepakDecoder::new();
    assert!(
        dec_sat
            .decode_deviant_frame(&frame_bytes, None, DeviantConfig::saturn())
            .is_err(),
        "Saturn config must reject a Lemmings-3DO frame (6 extra hdr bytes ≠ 2)"
    );

    // Lemmings 3DO config accepts.
    let mut dec_3do = CinepakDecoder::new();
    let frame = dec_3do
        .decode_deviant_frame(&frame_bytes, None, DeviantConfig::lemmings_3do())
        .expect("Lemmings 3DO deviant decode must succeed");
    assert_eq!(frame.width, 4);
    assert_eq!(frame.height, 4);
    let p = frame.pixels();
    assert_eq!(p[0], expected_y0);
}

#[test]
fn deviant_config_constructors_match_spec_documented_values() {
    let s = DeviantConfig::saturn();
    assert_eq!(s.extra_header_bytes, 2);
    assert_eq!(s.frame_length_short_by, 8);
    assert!(s.tolerate_codebook_pad);

    let l = DeviantConfig::lemmings_3do();
    assert_eq!(l.extra_header_bytes, 6);
    assert_eq!(l.frame_length_short_by, 8);
    assert!(l.tolerate_codebook_pad);
}

/// Standard Cinepak frames should still decode through the standard
/// `decode_frame` path after the deviant refactor. Regression check
/// that we didn't break the happy path.
#[test]
fn standard_decoder_still_works() {
    // Build a clean 4×4 single-MB V1 intra frame (no deviations).
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0, 0, 0, 0, 0, 4, 0, 4, 0, 1]); // frame hdr
    let strip_hdr_pos = bytes.len();
    bytes.extend_from_slice(&[0x10, 0x00, 0, 0, 0, 0, 0, 0, 0, 4, 0, 4]); // strip hdr
    bytes.extend_from_slice(&[0x20, 0x00, 0x00, 0x04]); // empty V4
    bytes.extend_from_slice(&[0x22, 0x00, 0x00, 0x0a]); // V1 full, 1 entry
    bytes.extend_from_slice(&[80, 90, 100, 110, 0, 0]);
    bytes.extend_from_slice(&[0x32, 0x00, 0x00, 0x05]); // vector chunk
    bytes.push(0); // V1 idx 0

    let strip_size = (bytes.len() - strip_hdr_pos) as u16;
    bytes[strip_hdr_pos + 2..strip_hdr_pos + 4].copy_from_slice(&strip_size.to_be_bytes());
    let fl = bytes.len() as u32;
    bytes[1] = ((fl >> 16) & 0xff) as u8;
    bytes[2] = ((fl >> 8) & 0xff) as u8;
    bytes[3] = (fl & 0xff) as u8;

    let mut dec = CinepakDecoder::new();
    let frame = dec.decode_frame(&bytes, None).expect("standard decode");
    assert_eq!(frame.width, 4);
    let p = frame.pixels();
    assert_eq!(p[0], 80); // Y0
}

/// `FilmDemuxer::variant()` classifies the wire-format variant from
/// header inspection alone. Tests the classifier against the
/// `Sega_FILM.wiki` decision rules (lines 200–224).
#[test]
fn film_demuxer_classifies_variant_saturn() {
    let mut out = Vec::new();
    // FILM header.
    out.extend_from_slice(b"FILM");
    out.extend_from_slice(&80u32.to_be_bytes()); // header_length
    out.extend_from_slice(b"1.09"); // ASCII version → Saturn
    out.extend_from_slice(&0u32.to_be_bytes());
    // FDSC 32-byte.
    out.extend_from_slice(b"FDSC");
    out.extend_from_slice(&0x20u32.to_be_bytes());
    out.extend_from_slice(b"cvid");
    out.extend_from_slice(&120u32.to_be_bytes()); // h
    out.extend_from_slice(&160u32.to_be_bytes()); // w
    out.push(24);
    out.extend_from_slice(&[0u8; 11]);
    // STAB minimal.
    out.extend_from_slice(b"STAB");
    out.extend_from_slice(&16u32.to_be_bytes());
    out.extend_from_slice(&30u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // 0 entries

    let dem = oxideav_cinepak::FilmDemuxer::parse(&out).unwrap();
    assert_eq!(dem.variant(), CinepakVariant::DeviantSaturn);
}

#[test]
fn film_demuxer_classifies_variant_lemmings_3do() {
    // Lemmings 3DO: NULL version field + cvid codec.
    let mut out = Vec::new();
    out.extend_from_slice(b"FILM");
    out.extend_from_slice(&((16 + 0x14 + 16) as u32).to_be_bytes());
    out.extend_from_slice(&[0u8, 0, 0, 0]); // NULL version
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"FDSC");
    out.extend_from_slice(&0x14u32.to_be_bytes()); // abbreviated 20-byte
    out.extend_from_slice(b"cvid");
    out.extend_from_slice(&80u32.to_be_bytes()); // h
    out.extend_from_slice(&80u32.to_be_bytes()); // w
    out.extend_from_slice(b"STAB");
    out.extend_from_slice(&16u32.to_be_bytes());
    out.extend_from_slice(&30u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());

    let dem = oxideav_cinepak::FilmDemuxer::parse(&out).unwrap();
    assert_eq!(dem.variant(), CinepakVariant::DeviantLemmings3do);
}

#[test]
fn film_demuxer_classifies_variant_out_of_scope() {
    // 'sega' FOURCC → out of scope (Cinepak-for-Sega is a different
    // codec per `00-scope.md` §"Out of scope").
    let mut out = Vec::new();
    out.extend_from_slice(b"FILM");
    out.extend_from_slice(&80u32.to_be_bytes());
    out.extend_from_slice(b"1.04");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"FDSC");
    out.extend_from_slice(&0x20u32.to_be_bytes());
    out.extend_from_slice(b"sega");
    out.extend_from_slice(&120u32.to_be_bytes());
    out.extend_from_slice(&160u32.to_be_bytes());
    out.push(24);
    out.extend_from_slice(&[0u8; 11]);
    out.extend_from_slice(b"STAB");
    out.extend_from_slice(&16u32.to_be_bytes());
    out.extend_from_slice(&30u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());

    let dem = oxideav_cinepak::FilmDemuxer::parse(&out).unwrap();
    assert_eq!(dem.variant(), CinepakVariant::OutOfScope);
}

/// Multi-strip deviant frame: two strips, each with the 0x5FC
/// codebook anomaly. Exercises the chunk-walker's trailing-pad
/// handling across strip boundaries.
#[test]
fn decodes_deviant_multi_strip_frame() {
    // 4×8 frame, 2 strips of 4×4.
    let mut bytes = Vec::new();
    bytes.push(0x00); // flags
    bytes.extend_from_slice(&[0, 0, 0]); // frame_length placeholder
    bytes.extend_from_slice(&4u16.to_be_bytes()); // width
    bytes.extend_from_slice(&8u16.to_be_bytes()); // height
    bytes.extend_from_slice(&2u16.to_be_bytes()); // strip_count
    bytes.extend_from_slice(&[0, 0]); // 2-byte saturn pad

    // Helper to emit one strip with the deviant codebook anomaly.
    let mut emit_strip = |y_top: u16, y_bottom_raw: u16, y0: u8| {
        let strip_hdr_pos = bytes.len();
        bytes.extend_from_slice(&0x1000u16.to_be_bytes()); // intra
        bytes.extend_from_slice(&0u16.to_be_bytes()); // size placeholder
        bytes.extend_from_slice(&y_top.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes()); // x_top
        bytes.extend_from_slice(&y_bottom_raw.to_be_bytes()); // y_bottom or height
        bytes.extend_from_slice(&4u16.to_be_bytes()); // x_bottom
                                                      // Empty V4.
        bytes.extend_from_slice(&[0x20, 0x00, 0x00, 0x04]);
        // V1 deviant (255 entries + 2 pad).
        bytes.extend_from_slice(&0x2200u16.to_be_bytes());
        bytes.extend_from_slice(&0x0600u16.to_be_bytes());
        for i in 0..255u16 {
            if i == 0 {
                bytes.extend_from_slice(&[y0, y0, y0, y0, 0, 0]);
            } else {
                let y = (i & 0xff) as u8;
                bytes.extend_from_slice(&[y, y, y, y, 0, 0]);
            }
        }
        bytes.extend_from_slice(&[0xAB, 0xCD]);
        // Vector chunk: 1 MB referencing V1 entry 0.
        bytes.extend_from_slice(&[0x32, 0x00, 0x00, 0x05, 0x00]);
        // Patch strip size.
        let strip_size = (bytes.len() - strip_hdr_pos) as u16;
        bytes[strip_hdr_pos + 2..strip_hdr_pos + 4].copy_from_slice(&strip_size.to_be_bytes());
    };
    emit_strip(0, 4, 60); // strip 0: y=0..4, y0=60
                          // strip 1: y_top=0 sentinel → resolves to prev_y_bottom=4; y_bottom_raw=4 = height
    emit_strip(0, 4, 200);

    // Patch frame_length to (real_len - 8) per deviant convention.
    let real = bytes.len() as u32;
    let declared = real - 8;
    bytes[1] = ((declared >> 16) & 0xff) as u8;
    bytes[2] = ((declared >> 8) & 0xff) as u8;
    bytes[3] = (declared & 0xff) as u8;

    let mut dec = CinepakDecoder::new();
    let frame = dec
        .decode_deviant_frame(&bytes, None, DeviantConfig::saturn())
        .expect("multi-strip deviant decode");
    assert_eq!(frame.width, 4);
    assert_eq!(frame.height, 8);
    let p = frame.pixels();
    // Strip 0 row 0 col 0 → Y0=60.
    assert_eq!(p[0], 60);
    // Strip 1 row 4 col 0 → Y0=200.
    let row4 = 4 * frame.stride();
    assert_eq!(p[row4], 200);
}
