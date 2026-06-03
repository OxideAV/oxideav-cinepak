//! Round 228 — FILM linear-PCM sample-data shaping helpers.
//!
//! Verifies the round-228 helpers convert the FILM-wire bytes
//! (sign-magnitude / big-endian / non-interleaved-stereo) into the
//! shape a standard PCM sink expects (twos-complement / host-endian /
//! LR-interleaved). All test data is derived directly from the
//! encoding rules quoted verbatim in `docs/video/cinepak/reference/
//! wiki/Sega_FILM.wiki` lines 147–169.

use oxideav_cinepak::{
    pcm_decode_16be_to_i16, pcm_decode_8bit, pcm_deinterleave_stereo_16be,
    pcm_deinterleave_stereo_8bit, pcm_sign_magnitude_to_i8, FilmAudioFormat, PcmEndianness,
    PcmSignConvention,
};

// ---- pcm_sign_magnitude_to_i8 --------------------------------------

#[test]
fn sign_magnitude_matches_wiki_examples() {
    // Wiki lines 167–169: "0x81 represents -1 and 0xFF represents -127.
    // 0x01 and 0x7F still represent 1 and 127, respectively".
    assert_eq!(pcm_sign_magnitude_to_i8(0x01), 1);
    assert_eq!(pcm_sign_magnitude_to_i8(0x7F), 127);
    assert_eq!(pcm_sign_magnitude_to_i8(0x81), -1);
    assert_eq!(pcm_sign_magnitude_to_i8(0xFF), -127);
}

#[test]
fn sign_magnitude_zeros_collapse() {
    // 0x00 = +0; 0x80 = -0; both collapse to 0i8 (twos-complement
    // has no -0 representation).
    assert_eq!(pcm_sign_magnitude_to_i8(0x00), 0);
    assert_eq!(pcm_sign_magnitude_to_i8(0x80), 0);
}

#[test]
fn sign_magnitude_full_byte_range_is_total() {
    // Every input byte maps to some i8; the function is total.
    for b in 0u8..=255 {
        let s = pcm_sign_magnitude_to_i8(b);
        // |s| ≤ 127 always (the 7-bit magnitude is bits 6..0).
        assert!((-127..=127).contains(&s), "byte {b:#04x} -> {s}");
    }
}

// ---- pcm_decode_8bit ----------------------------------------------

#[test]
fn pcm_decode_8bit_twos_complement_is_bitcast() {
    let src = [0x00u8, 0x01, 0x7F, 0x80, 0xFF];
    let mut dst = [0i8; 5];
    pcm_decode_8bit(&src, PcmSignConvention::TwosComplement, &mut dst).unwrap();
    // Twos-complement: 0x00→0, 0x01→1, 0x7F→127, 0x80→-128, 0xFF→-1.
    assert_eq!(dst, [0, 1, 127, -128, -1]);
}

#[test]
fn pcm_decode_8bit_sign_magnitude_applies_rule() {
    let src = [0x00u8, 0x01, 0x7F, 0x80, 0x81, 0xFF];
    let mut dst = [0i8; 6];
    pcm_decode_8bit(&src, PcmSignConvention::SignMagnitude, &mut dst).unwrap();
    // Per wiki: 0x80 -> -0 -> 0; 0x81 -> -1; 0xFF -> -127.
    assert_eq!(dst, [0, 1, 127, 0, -1, -127]);
}

#[test]
fn pcm_decode_8bit_length_mismatch_rejected() {
    let src = [0u8; 4];
    let mut dst = [0i8; 3];
    assert!(pcm_decode_8bit(&src, PcmSignConvention::TwosComplement, &mut dst).is_err());
}

#[test]
fn pcm_decode_8bit_empty_is_ok() {
    let src: [u8; 0] = [];
    let mut dst: [i8; 0] = [];
    pcm_decode_8bit(&src, PcmSignConvention::TwosComplement, &mut dst).unwrap();
    pcm_decode_8bit(&src, PcmSignConvention::SignMagnitude, &mut dst).unwrap();
}

// ---- pcm_decode_16be_to_i16 ---------------------------------------

#[test]
fn pcm_decode_16be_reads_big_endian() {
    // Wiki line 153: 16-bit FILM PCM is big-endian.
    // 0x0001 BE = 1; 0x7FFF = 32767; 0x8000 = -32768; 0xFFFF = -1.
    let src = [
        0x00u8, 0x00, // 0
        0x00, 0x01, // 1
        0x7F, 0xFF, // 32767
        0x80, 0x00, // -32768
        0xFF, 0xFF, // -1
    ];
    let mut dst = [0i16; 5];
    pcm_decode_16be_to_i16(&src, &mut dst).unwrap();
    assert_eq!(dst, [0, 1, 32767, -32768, -1]);
}

#[test]
fn pcm_decode_16be_rejects_odd_length() {
    let src = [0u8; 5];
    let mut dst = [0i16; 2];
    assert!(pcm_decode_16be_to_i16(&src, &mut dst).is_err());
}

#[test]
fn pcm_decode_16be_rejects_dst_size_mismatch() {
    let src = [0u8; 6];
    let mut dst = [0i16; 4];
    assert!(pcm_decode_16be_to_i16(&src, &mut dst).is_err());
}

#[test]
fn pcm_decode_16be_empty_is_ok() {
    let src: [u8; 0] = [];
    let mut dst: [i16; 0] = [];
    pcm_decode_16be_to_i16(&src, &mut dst).unwrap();
}

// ---- pcm_deinterleave_stereo_8bit ---------------------------------

#[test]
fn deinterleave_stereo_8bit_matches_wiki_split() {
    // Wiki lines 156–160: "for each audio data chunk, the first half
    // of the chunk contains left channel samples and the second half
    // contains right channel samples." Source = LLLL RRRR.
    let src = [0x10u8, 0x11, 0x12, 0x13, 0x20, 0x21, 0x22, 0x23];
    let mut dst = [0u8; 8];
    pcm_deinterleave_stereo_8bit(&src, &mut dst).unwrap();
    // Output should be L0 R0 L1 R1 L2 R2 L3 R3.
    assert_eq!(dst, [0x10, 0x20, 0x11, 0x21, 0x12, 0x22, 0x13, 0x23]);
}

#[test]
fn deinterleave_stereo_8bit_rejects_odd_length() {
    let src = [0u8; 5];
    let mut dst = [0u8; 5];
    assert!(pcm_deinterleave_stereo_8bit(&src, &mut dst).is_err());
}

#[test]
fn deinterleave_stereo_8bit_rejects_size_mismatch() {
    let src = [0u8; 6];
    let mut dst = [0u8; 5];
    assert!(pcm_deinterleave_stereo_8bit(&src, &mut dst).is_err());
}

#[test]
fn deinterleave_stereo_8bit_empty_is_ok() {
    let src: [u8; 0] = [];
    let mut dst: [u8; 0] = [];
    pcm_deinterleave_stereo_8bit(&src, &mut dst).unwrap();
}

#[test]
fn deinterleave_stereo_8bit_does_not_transform_samples() {
    // Confirm the helper does NOT apply sign/magnitude conversion —
    // it is documented as pure re-shuffling.
    let src = [0x81u8, 0xFF, 0x01, 0x7F];
    let mut dst = [0u8; 4];
    pcm_deinterleave_stereo_8bit(&src, &mut dst).unwrap();
    // L0=0x81, L1=0xFF, R0=0x01, R1=0x7F  →  L0 R0 L1 R1.
    assert_eq!(dst, [0x81, 0x01, 0xFF, 0x7F]);
}

// ---- pcm_deinterleave_stereo_16be ---------------------------------

#[test]
fn deinterleave_stereo_16be_combines_decode_and_interleave() {
    // 4 stereo samples (8 i16 values, 16 bytes): the wire is
    // L0_BE L1_BE L2_BE L3_BE | R0_BE R1_BE R2_BE R3_BE.
    let src = [
        // Left half: L0=0x0001 L1=0x0002 L2=0x0003 L3=0x7FFF
        0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x7F, 0xFF,
        // Right half: R0=0xFFFE(-2) R1=0xFFFC(-4) R2=0xFFFA(-6) R3=0x8000(-32768)
        0xFF, 0xFE, 0xFF, 0xFC, 0xFF, 0xFA, 0x80, 0x00,
    ];
    let mut dst = [0i16; 8];
    pcm_deinterleave_stereo_16be(&src, &mut dst).unwrap();
    // Expected interleaved: L0 R0 L1 R1 L2 R2 L3 R3.
    assert_eq!(dst, [1, -2, 2, -4, 3, -6, 32767, -32768]);
}

#[test]
fn deinterleave_stereo_16be_rejects_non_multiple_of_four() {
    let src = [0u8; 6];
    let mut dst = [0i16; 3];
    assert!(pcm_deinterleave_stereo_16be(&src, &mut dst).is_err());
}

#[test]
fn deinterleave_stereo_16be_rejects_size_mismatch() {
    let src = [0u8; 8];
    let mut dst = [0i16; 3];
    assert!(pcm_deinterleave_stereo_16be(&src, &mut dst).is_err());
}

#[test]
fn deinterleave_stereo_16be_empty_is_ok() {
    let src: [u8; 0] = [];
    let mut dst: [i16; 0] = [];
    pcm_deinterleave_stereo_16be(&src, &mut dst).unwrap();
}

// ---- FilmAudioFormat::decode_chunk_to_i16 -------------------------

#[test]
fn audio_format_decode_chunk_to_i16_mono_8bit_saturn() {
    // Saturn = TwosComplement per the FILM-version → convention map.
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 1,
        bits_per_sample: 8,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::NotApplicable,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    let src = [0x00u8, 0x7F, 0x80, 0xFF];
    let out = fmt.decode_chunk_to_i16(&src).expect("LinearPcm decodes");
    // Twos-complement: 0x80 ⇒ -128; 0xFF ⇒ -1.
    assert_eq!(out, vec![0i16, 127, -128, -1]);
}

#[test]
fn audio_format_decode_chunk_to_i16_mono_8bit_sega_cd() {
    // Sega CD = SignMagnitude.
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 1,
        bits_per_sample: 8,
        sample_rate_hz: 16000,
        endianness: PcmEndianness::NotApplicable,
        sign_convention: PcmSignConvention::SignMagnitude,
    };
    let src = [0x01u8, 0x7F, 0x81, 0xFF];
    let out = fmt.decode_chunk_to_i16(&src).expect("decode mono 8bit");
    // Sign/magnitude: 0x81 ⇒ -1; 0xFF ⇒ -127.
    assert_eq!(out, vec![1i16, 127, -1, -127]);
}

#[test]
fn audio_format_decode_chunk_to_i16_stereo_8bit_interleaves() {
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 2,
        bits_per_sample: 8,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::NotApplicable,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    // L0 L1 L2 L3 | R0 R1 R2 R3 ⇒ L0 R0 L1 R1 L2 R2 L3 R3.
    let src = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x7F];
    let out = fmt.decode_chunk_to_i16(&src).unwrap();
    assert_eq!(out, vec![0x10i16, 0x50, 0x20, 0x60, 0x30, 0x70, 0x40, 0x7F]);
}

#[test]
fn audio_format_decode_chunk_to_i16_stereo_8bit_sign_magnitude() {
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 2,
        bits_per_sample: 8,
        sample_rate_hz: 16000,
        endianness: PcmEndianness::NotApplicable,
        sign_convention: PcmSignConvention::SignMagnitude,
    };
    let src = [0x01u8, 0x7F, 0x81, 0xFF]; // L=[+1,+127] R=[-1,-127]
    let out = fmt.decode_chunk_to_i16(&src).unwrap();
    assert_eq!(out, vec![1i16, -1, 127, -127]);
}

#[test]
fn audio_format_decode_chunk_to_i16_mono_16bit() {
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 1,
        bits_per_sample: 16,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::BigEndian,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    let src = [0x00u8, 0x01, 0x7F, 0xFF, 0x80, 0x00, 0xFF, 0xFF];
    let out = fmt.decode_chunk_to_i16(&src).unwrap();
    assert_eq!(out, vec![1i16, 32767, -32768, -1]);
}

#[test]
fn audio_format_decode_chunk_to_i16_stereo_16bit() {
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 2,
        bits_per_sample: 16,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::BigEndian,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    // 2 stereo samples: L0=1 L1=2 | R0=3 R1=-1 (8 bytes total).
    let src = [
        0x00, 0x01, 0x00, 0x02, // left
        0x00, 0x03, 0xFF, 0xFF, // right
    ];
    let out = fmt.decode_chunk_to_i16(&src).unwrap();
    // Interleaved: L0 R0 L1 R1 = 1 3 2 -1.
    assert_eq!(out, vec![1i16, 3, 2, -1]);
}

#[test]
fn audio_format_decode_chunk_to_i16_returns_none_for_non_linear_pcm() {
    let fmt = FilmAudioFormat::CriAdxAdpcm {
        channels: 2,
        sample_rate_hz: 22050,
    };
    assert!(fmt.decode_chunk_to_i16(&[0u8; 8]).is_none());

    let fmt = FilmAudioFormat::None;
    assert!(fmt.decode_chunk_to_i16(&[0u8; 8]).is_none());

    let fmt = FilmAudioFormat::Unknown {
        channels: 1,
        bits_per_sample: 8,
        sample_rate_hz: 11025,
        compression: 7,
    };
    assert!(fmt.decode_chunk_to_i16(&[0u8; 4]).is_none());
}

#[test]
fn audio_format_decode_chunk_to_i16_returns_none_for_unsupported_combos() {
    // 4-channel PCM is not in the wiki's documented combos.
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 4,
        bits_per_sample: 8,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::NotApplicable,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    assert!(fmt.decode_chunk_to_i16(&[0u8; 16]).is_none());

    // 24-bit PCM is not documented in the wiki.
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 2,
        bits_per_sample: 24,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::BigEndian,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    assert!(fmt.decode_chunk_to_i16(&[0u8; 12]).is_none());
}

#[test]
fn audio_format_decode_chunk_to_i16_returns_none_for_bad_size() {
    // Stereo 8-bit needs even length.
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 2,
        bits_per_sample: 8,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::NotApplicable,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    assert!(fmt.decode_chunk_to_i16(&[0u8; 5]).is_none());

    // Stereo 16-bit needs multiple of 4.
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 2,
        bits_per_sample: 16,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::BigEndian,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    assert!(fmt.decode_chunk_to_i16(&[0u8; 6]).is_none());

    // Mono 16-bit needs even length.
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 1,
        bits_per_sample: 16,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::BigEndian,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    assert!(fmt.decode_chunk_to_i16(&[0u8; 5]).is_none());
}

#[test]
fn audio_format_decode_chunk_to_i16_empty_chunk_decodes_to_empty_vec() {
    let fmt = FilmAudioFormat::LinearPcm {
        channels: 1,
        bits_per_sample: 8,
        sample_rate_hz: 22050,
        endianness: PcmEndianness::NotApplicable,
        sign_convention: PcmSignConvention::TwosComplement,
    };
    let out = fmt.decode_chunk_to_i16(&[]).unwrap();
    assert!(out.is_empty());
}
