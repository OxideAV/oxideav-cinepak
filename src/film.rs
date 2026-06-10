//! Sega FILM (CPK) container demuxer.
//!
//! Wire-format reference: `docs/video/cinepak/spec/05-container-carriage.md` §2.
//!
//! The Sega FILM container is a flat header-then-data layout, fully
//! big-endian (designed for the Saturn's M68K-derived processor). This
//! module provides:
//!
//! - [`probe_film`] — confirm the `FILM` signature without parsing.
//! - [`FilmHeader`] — the outer header (signature, length, version).
//! - [`Fdsc`] — the File Description chunk (codec FOURCC + dims +
//!   audio metadata).
//! - [`Stab`] — the Sample Table chunk (per-sample offsets, lengths,
//!   keyframe bits, timestamps).
//! - [`FilmDemuxer::parse`] — parse a complete header buffer (the
//!   first `header_length` bytes of a `.cpk` file) and return the
//!   sample-table records ready for sample extraction.
//!
//! ## Scope
//!
//! This module parses **non-deviant** FILM files (FFmpeg `film_cpk`
//! output, fixture `C3` in the spec). The Saturn-specific deviant
//! variant (§2.6) is recognised — `extra_per_frame_bytes` is exposed —
//! but the deviant codec-frame slicing is deferred until a genuine
//! Saturn fixture is acquired (§2.8).
//!
//! Audio samples (`sample_info_1 == 0xFFFFFFFF`) are surfaced through
//! [`SampleRecord::is_audio`] but their codec-side decoding is out of
//! scope for `oxideav-cinepak`.

use crate::error::{CinepakError, Result};

pub const FILM_HEADER_MIN_SIZE: usize = 16;
pub const FDSC_SIGNATURE: &[u8; 4] = b"FDSC";
pub const STAB_SIGNATURE: &[u8; 4] = b"STAB";
pub const FILM_SIGNATURE: &[u8; 4] = b"FILM";

/// Cheap signature-only probe. Returns `true` iff `bytes` begins with
/// the four ASCII characters `'FILM'`. Use this from a probe-fn or
/// container-dispatch site without paying for a full header parse.
pub fn probe_film(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == FILM_SIGNATURE
}

/// The outer 16-byte FILM file header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilmHeader {
    /// Total length of the FILM header (FILM + FDSC + STAB chunks).
    /// Equal to the file offset of the start of the sample data block.
    pub header_length: u32,
    /// Format version in 4-byte ASCII (e.g. `'1.09'`). May be
    /// `0x00000000` or `0x00020000` for early variants.
    pub version: [u8; 4],
    /// Reserved 4 bytes; usually zero.
    pub reserved: u32,
}

impl FilmHeader {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < FILM_HEADER_MIN_SIZE {
            return Err(CinepakError::invalid(format!(
                "FILM: header truncated ({} < {FILM_HEADER_MIN_SIZE})",
                bytes.len()
            )));
        }
        if &bytes[0..4] != FILM_SIGNATURE {
            return Err(CinepakError::invalid(
                "FILM: missing 'FILM' signature at offset 0",
            ));
        }
        let header_length = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        let mut version = [0u8; 4];
        version.copy_from_slice(&bytes[8..12]);
        let reserved = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        if (header_length as usize) < FILM_HEADER_MIN_SIZE {
            return Err(CinepakError::invalid(format!(
                "FILM: header_length {header_length} smaller than 16-byte minimum"
            )));
        }
        Ok(Self {
            header_length,
            version,
            reserved,
        })
    }
}

/// File Description chunk (`FDSC`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fdsc {
    /// FOURCC of the video codec. `'cvid'` for standard Cinepak;
    /// `'sega'` and `'Seg4'` are out of scope per spec §2.3.
    pub video_codec: [u8; 4],
    /// Frame height in pixels.
    pub height: u32,
    /// Frame width in pixels. Note: in the wire format, height
    /// precedes width — the parser undoes this oddity.
    pub width: u32,
    pub bits_per_pixel: u8,
    pub audio_channels: u8,
    pub audio_bits: u8,
    /// `0` = linear PCM; `2` = CRI ADX ADPCM (out of scope).
    pub audio_compression: u8,
    pub audio_sample_rate: u16,
}

/// Which Cinepak wire-format variant a FILM container's video samples
/// use. Determined from the FILM header version + FDSC chunk layout
/// per `Sega_FILM.wiki` lines 125–224.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CinepakVariant {
    /// Standard Cinepak (`'cvid'` FOURCC, ASCII version field
    /// `'1.0X'` or `'1.0Y'`). Decode with
    /// [`crate::CinepakDecoder::decode_frame`].
    Standard,
    /// Sega Saturn / Sega CD `'cvid'` deviant variant. 12-byte
    /// frame-header prefix, `frame_length` short by 8, codebook
    /// chunks may have trailing pad. Decode with
    /// [`crate::CinepakDecoder::decode_deviant_frame`] +
    /// [`crate::DeviantConfig::saturn`].
    DeviantSaturn,
    /// Lemmings 3DO `'cvid'` deviant variant. 16-byte frame-header
    /// prefix (6 extra bytes after the standard 10), otherwise the
    /// same as `DeviantSaturn`. Detected by NULL version field +
    /// `'cvid'` FOURCC per `Sega_FILM.wiki` line 189.
    DeviantLemmings3do,
    /// `'sega'` / `'Seg4'` Cinepak-for-Sega variant — distinct
    /// codec, out of scope for this crate per spec §2.3 of
    /// `00-scope.md`.
    OutOfScope,
}

impl Fdsc {
    /// Parse an `FDSC` chunk starting at `bytes[0..]`. Returns the
    /// parsed `Fdsc` plus the chunk's `chunk_length` field (which is
    /// inclusive of the 8-byte signature+length header).
    pub fn parse(bytes: &[u8]) -> Result<(Self, u32)> {
        if bytes.len() < 8 {
            return Err(CinepakError::invalid("FILM: FDSC chunk header truncated"));
        }
        if &bytes[0..4] != FDSC_SIGNATURE {
            return Err(CinepakError::invalid(format!(
                "FILM: expected 'FDSC' signature, got {:?}",
                &bytes[0..4]
            )));
        }
        let chunk_length = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        // Standard layout is 32 bytes; abbreviated 20-byte layout exists
        // for early Sega CD variants. We accept both.
        let cl = chunk_length as usize;
        if cl != 0x20 && cl != 0x14 {
            return Err(CinepakError::invalid(format!(
                "FILM: FDSC chunk_length must be 0x20 or 0x14, got 0x{chunk_length:x}"
            )));
        }
        if bytes.len() < cl {
            return Err(CinepakError::invalid(format!(
                "FILM: FDSC chunk_length {cl} exceeds buffer {}",
                bytes.len()
            )));
        }
        let mut video_codec = [0u8; 4];
        video_codec.copy_from_slice(&bytes[8..12]);
        let height = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        if cl == 0x14 {
            // Abbreviated form: no audio fields.
            return Ok((
                Self {
                    video_codec,
                    height,
                    width,
                    bits_per_pixel: 0,
                    audio_channels: 0,
                    audio_bits: 0,
                    audio_compression: 0,
                    audio_sample_rate: 0,
                },
                chunk_length,
            ));
        }
        let bits_per_pixel = bytes[20];
        let audio_channels = bytes[21];
        let audio_bits = bytes[22];
        let audio_compression = bytes[23];
        let audio_sample_rate = u16::from_be_bytes(bytes[24..26].try_into().unwrap());
        // bytes[26..32] are reserved.
        Ok((
            Self {
                video_codec,
                height,
                width,
                bits_per_pixel,
                audio_channels,
                audio_bits,
                audio_compression,
                audio_sample_rate,
            },
            chunk_length,
        ))
    }

    /// Returns `true` for codecs this crate is in scope for. The
    /// `'sega'` and `'Seg4'` Cinepak-for-Sega variants are out of
    /// scope per spec §2.3.
    pub fn is_standard_cinepak(&self) -> bool {
        &self.video_codec == b"cvid"
    }

    /// `true` iff the FDSC declares an audio stream. Per
    /// `Sega_FILM.wiki` line 82 ("The fields pertaining to audio
    /// (channels, bits, compression, and sample rate) will all be 0
    /// if there is no audio present in the file"), `has_audio()`
    /// detects the all-zero sentinel — the abbreviated 20-byte FDSC
    /// (early Sega CD variants, no audio fields) also reports `false`
    /// because [`Fdsc::parse`] zero-fills the audio fields on the
    /// abbreviated form.
    pub fn has_audio(&self) -> bool {
        self.audio_channels != 0
            || self.audio_bits != 0
            || self.audio_compression != 0
            || self.audio_sample_rate != 0
    }

    /// Classify the audio stream this FDSC describes. Returns a
    /// structured [`FilmAudioFormat`] per `Sega_FILM.wiki` lines
    /// 147–169 — covers the linear-PCM payload encoding (byte order,
    /// sign convention), the CRI ADX ADPCM variant (`audio_compression
    /// = 2`, wiki line 149), the no-audio sentinel, and the unknown
    /// `audio_compression` discriminator.
    ///
    /// `film_version` is the [`FilmHeader::version`] field — required
    /// because the audio sign convention depends on the platform: per
    /// wiki line 151 Saturn (ASCII version `'1.0X'`) stores signed
    /// twos-complement PCM, while Sega CD (NULL version, wiki line
    /// 162 + 224) stores sign/magnitude PCM. Callers who have a full
    /// demuxer should prefer [`FilmDemuxer::audio_format`] which
    /// threads the version through automatically.
    pub fn audio_format(&self, film_version: &[u8; 4]) -> FilmAudioFormat {
        if !self.has_audio() {
            return FilmAudioFormat::None;
        }
        match self.audio_compression {
            0 => {
                // Linear PCM. Byte-order + sign-convention rules per
                // wiki lines 151, 153, 162.
                let endianness = match self.audio_bits {
                    16 => PcmEndianness::BigEndian,
                    _ => PcmEndianness::NotApplicable,
                };
                let sign = pcm_sign_convention_for(film_version);
                FilmAudioFormat::LinearPcm {
                    channels: self.audio_channels,
                    bits_per_sample: self.audio_bits,
                    sample_rate_hz: self.audio_sample_rate,
                    endianness,
                    sign_convention: sign,
                }
            }
            2 => FilmAudioFormat::CriAdxAdpcm {
                channels: self.audio_channels,
                sample_rate_hz: self.audio_sample_rate,
            },
            other => FilmAudioFormat::Unknown {
                channels: self.audio_channels,
                bits_per_sample: self.audio_bits,
                sample_rate_hz: self.audio_sample_rate,
                compression: other,
            },
        }
    }
}

/// Sign convention used for linear-PCM audio samples in a FILM file.
///
/// Per `Sega_FILM.wiki`:
/// - Line 151: Saturn `.cpk` files store signed (twos-complement) PCM.
/// - Line 162: Sega CD files store sign/magnitude PCM (bit 7 = sign,
///   bits 6..0 = magnitude — so `0x81` = -1, `0xFF` = -127).
///
/// The convention is inferred from the [`FilmHeader::version`] field —
/// ASCII versions like `'1.07'` / `'1.09'` are Saturn (twos-complement),
/// NULL or non-ASCII versions are early Sega CD / 3DO (sign/magnitude
/// is the documented Sega CD encoding; the 3DO Lemmings entry at wiki
/// line 189 says "8-bit signed, monaural PCM" without explicitly
/// specifying twos-complement vs sign/magnitude, but the same NULL-version
/// detection rule classifies them as Sega-CD-era files).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmSignConvention {
    /// Two's complement signed integers (Saturn `'1.0X'` ASCII versions).
    TwosComplement,
    /// Sign/magnitude: bit 7 is the sign bit, bits 6..0 are the
    /// magnitude. Only meaningful for 8-bit PCM per wiki line 162.
    SignMagnitude,
}

/// Byte order used for multi-byte PCM samples. Per `Sega_FILM.wiki`
/// line 153 ("16-bit audio data, the individual PCM samples are stored
/// in big endian format"), 16-bit FILM PCM is big-endian. 8-bit PCM
/// has no meaningful endianness, so the enum carries [`NotApplicable`]
/// for that case.
///
/// [`NotApplicable`]: PcmEndianness::NotApplicable
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmEndianness {
    BigEndian,
    NotApplicable,
}

/// Classified audio format for a FILM file's audio stream. Produced by
/// [`Fdsc::audio_format`] / [`FilmDemuxer::audio_format`]. Mirrors the
/// `audio_compression` discriminator in the FDSC chunk per
/// `Sega_FILM.wiki` line 149 plus the linear-PCM encoding rules per
/// wiki lines 151–162.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilmAudioFormat {
    /// No audio stream. The FDSC's audio fields are all zero (wiki
    /// line 82) or the FDSC is in the abbreviated 20-byte layout (wiki
    /// line 178, audio fields omitted entirely).
    None,
    /// `audio_compression = 0` — linear PCM, with byte-order and
    /// sign-convention rules per wiki lines 151–162. Stereo channel
    /// layout in the **sample data** is non-interleaved per wiki line
    /// 156–160 (first half left, second half right); the consumer is
    /// responsible for re-interleaving before passing to a typical PCM
    /// playback API.
    LinearPcm {
        channels: u8,
        bits_per_sample: u8,
        sample_rate_hz: u16,
        endianness: PcmEndianness,
        sign_convention: PcmSignConvention,
    },
    /// `audio_compression = 2` — CRI ADX ADPCM, out of scope for this
    /// crate per `docs/video/cinepak/spec/00-scope.md` §2.3.
    /// Surfaced so consumers can detect and route to a separate
    /// decoder rather than treat the audio bytes as linear PCM.
    CriAdxAdpcm { channels: u8, sample_rate_hz: u16 },
    /// `audio_compression` is a value not documented in
    /// `Sega_FILM.wiki` line 149. The raw fields are preserved so a
    /// caller can log or route on the unknown discriminator.
    Unknown {
        channels: u8,
        bits_per_sample: u8,
        sample_rate_hz: u16,
        compression: u8,
    },
}

impl FilmAudioFormat {
    /// Average PCM byte rate (bytes per second of decoded audio).
    /// Only defined for [`FilmAudioFormat::LinearPcm`] — ADX ADPCM and
    /// the Unknown discriminator return `None` because the wire
    /// payload is not directly proportional to a wall-clock rate
    /// without invoking the codec.
    ///
    /// Formula per wiki lines 75–77 + 156–160:
    /// `channels × (bits_per_sample / 8) × sample_rate_hz`.
    ///
    /// Returns `None` when any field is zero (degenerate stream) or
    /// when `bits_per_sample` isn't a multiple of 8.
    pub fn byte_rate_bps(&self) -> Option<u64> {
        match self {
            Self::LinearPcm {
                channels,
                bits_per_sample,
                sample_rate_hz,
                ..
            } => {
                if *channels == 0 || *bits_per_sample == 0 || *sample_rate_hz == 0 {
                    return None;
                }
                if bits_per_sample % 8 != 0 {
                    return None;
                }
                let bytes_per_sample = (*bits_per_sample as u64) / 8;
                Some(u64::from(*channels) * bytes_per_sample * u64::from(*sample_rate_hz))
            }
            _ => None,
        }
    }

    /// `true` iff this is a [`FilmAudioFormat::LinearPcm`] payload —
    /// the only audio format with a well-defined byte→sample mapping
    /// in this crate. ADX ADPCM and Unknown return `false`; consumers
    /// who want to route to a separate decoder should check this
    /// first.
    pub fn is_linear_pcm(&self) -> bool {
        matches!(self, Self::LinearPcm { .. })
    }
}

// ---------------------------------------------------------------------
// Round 228 — linear-PCM sample-data shaping helpers
// ---------------------------------------------------------------------
//
// `Sega_FILM.wiki` lines 147–169 describe three encoding rules for
// linear-PCM audio that the bytes the demuxer yields are subject to:
// (a) `lines 151, 162` — Saturn `.cpk` files store signed
// twos-complement; early Sega CD / 3DO files store **sign/magnitude**
// where bit 7 is the sign bit and bits 6..0 are the magnitude;
// (b) `line 153` — 16-bit PCM samples are stored in **big-endian** byte
// order; and (c) `lines 156–160` — stereo PCM is stored
// **non-interleaved per chunk**: the first half of each audio sample
// chunk is the left channel, the second half is the right channel.
//
// Round 221 added [`FilmAudioFormat`] to classify these properties from
// FDSC + the FILM version field. The helpers below close the natural
// follow-up gap noted in [`FilmAudioFormat::LinearPcm`]'s docstring
// ("the consumer is responsible for re-interleaving before passing to a
// typical PCM playback API") by shaping the FILM-wire bytes into the
// format a standard PCM sink expects (twos-complement, host-endian,
// LR-interleaved). These are pure byte transformations — no audio
// decoding, no rate conversion, no channel mixing — and stay aligned
// with `00-scope.md` ("Audio codec decoding stays out of scope for
// this crate") because they operate on data the wiki already documents
// as **linear PCM**, not as a codec wire format.

/// Convert one 8-bit sample from FILM **sign/magnitude** encoding to
/// standard two's-complement [`i8`] per `Sega_FILM.wiki` lines 163–169.
///
/// Sign/magnitude rule:
/// - bit 7 set ⇒ negative; magnitude = bits 6..0 (so `0x81 = -1`,
///   `0xFF = -127`).
/// - bit 7 clear ⇒ non-negative; value = bits 6..0 directly (so
///   `0x01 = 1`, `0x7F = 127`).
///
/// Special case: `0x80` is "negative zero" in sign/magnitude — this
/// helper maps it to `0i8` since two's complement has no negative-zero
/// representation. Producers that care to distinguish `+0` from `-0`
/// must inspect the raw byte directly.
pub fn pcm_sign_magnitude_to_i8(b: u8) -> i8 {
    let sign = (b & 0x80) != 0;
    let magnitude = (b & 0x7F) as i8;
    if sign {
        // `0x80` ⇒ `-0` ⇒ collapses to `0`; otherwise `-magnitude`.
        -magnitude
    } else {
        magnitude
    }
}

/// Decode an 8-bit FILM PCM payload to host-side signed bytes,
/// applying the documented sign convention for the file's
/// [`FilmHeader::version`] per `Sega_FILM.wiki` lines 151, 162.
/// `dst.len()` must equal `src.len()`.
///
/// - [`PcmSignConvention::TwosComplement`] (Saturn ASCII versions) — a
///   byte-for-byte bitcast (the bytes are already two's complement).
/// - [`PcmSignConvention::SignMagnitude`] (Sega CD / 3DO NULL versions)
///   — every byte is run through [`pcm_sign_magnitude_to_i8`].
///
/// Returns `Err` when `dst.len() != src.len()`.
pub fn pcm_decode_8bit(src: &[u8], convention: PcmSignConvention, dst: &mut [i8]) -> Result<()> {
    if dst.len() != src.len() {
        return Err(CinepakError::invalid(format!(
            "FILM PCM: 8-bit decode length mismatch (src {} != dst {})",
            src.len(),
            dst.len(),
        )));
    }
    match convention {
        PcmSignConvention::TwosComplement => {
            for (i, &b) in src.iter().enumerate() {
                dst[i] = b as i8;
            }
        }
        PcmSignConvention::SignMagnitude => {
            for (i, &b) in src.iter().enumerate() {
                dst[i] = pcm_sign_magnitude_to_i8(b);
            }
        }
    }
    Ok(())
}

/// Decode a 16-bit FILM PCM payload to host-side [`i16`] per
/// `Sega_FILM.wiki` line 153 (16-bit FILM PCM is stored big-endian).
/// `dst.len()` must equal `src.len() / 2`, and `src.len()` must be
/// even.
///
/// 16-bit FILM PCM is documented as twos-complement on Saturn (the
/// only platform observed shipping 16-bit FILM audio per the wiki's
/// game list), so this helper performs only the big-endian → host
/// conversion. A `convention` argument is intentionally absent — the
/// wiki does not document a 16-bit sign/magnitude FILM variant.
pub fn pcm_decode_16be_to_i16(src: &[u8], dst: &mut [i16]) -> Result<()> {
    if src.len() % 2 != 0 {
        return Err(CinepakError::invalid(format!(
            "FILM PCM: 16-bit BE source length must be even, got {}",
            src.len(),
        )));
    }
    let samples = src.len() / 2;
    if dst.len() != samples {
        return Err(CinepakError::invalid(format!(
            "FILM PCM: 16-bit decode length mismatch (src {} bytes ⇒ {} samples != dst {})",
            src.len(),
            samples,
            dst.len(),
        )));
    }
    for (i, pair) in src.chunks_exact(2).enumerate() {
        dst[i] = i16::from_be_bytes([pair[0], pair[1]]);
    }
    Ok(())
}

/// Re-interleave one FILM stereo PCM chunk's 8-bit samples from the
/// wire's L-then-R half-chunk layout (`Sega_FILM.wiki` lines 156–160)
/// to the standard `L R L R …` interleave a typical PCM sink expects.
/// `src.len()` must be even (one byte per channel-sample); `dst.len()`
/// must equal `src.len()`.
///
/// The wire layout is:
///
/// ```text
///   src = [ L0 L1 L2 … L(n-1) R0 R1 R2 … R(n-1) ]
/// ```
///
/// The interleaved layout is:
///
/// ```text
///   dst = [ L0 R0 L1 R1 L2 R2 … L(n-1) R(n-1) ]
/// ```
///
/// Sample values are **not** transformed (no sign conversion); use
/// [`pcm_decode_8bit`] first if a sign-magnitude→twos-complement step
/// is also needed, then re-interleave the decoded `i8` slice.
pub fn pcm_deinterleave_stereo_8bit(src: &[u8], dst: &mut [u8]) -> Result<()> {
    if src.len() % 2 != 0 {
        return Err(CinepakError::invalid(format!(
            "FILM PCM: stereo 8-bit chunk length must be even, got {}",
            src.len(),
        )));
    }
    if dst.len() != src.len() {
        return Err(CinepakError::invalid(format!(
            "FILM PCM: stereo 8-bit interleave length mismatch (src {} != dst {})",
            src.len(),
            dst.len(),
        )));
    }
    let per_channel = src.len() / 2;
    let (left, right) = src.split_at(per_channel);
    for i in 0..per_channel {
        dst[2 * i] = left[i];
        dst[2 * i + 1] = right[i];
    }
    Ok(())
}

/// 16-bit analog of [`pcm_deinterleave_stereo_8bit`]: re-interleave a
/// FILM stereo PCM chunk's 16-bit big-endian samples from the wire's
/// L-then-R half-chunk layout (per `Sega_FILM.wiki` line 153 for
/// endianness + lines 156–160 for the L/R half-chunk split) into
/// host-endian `L R L R …` [`i16`] pairs.
///
/// `src.len()` must be a non-zero multiple of 4 (two bytes per sample
/// × two channels); `dst.len()` must equal `src.len() / 2`.
///
/// This is a one-shot helper that combines [`pcm_decode_16be_to_i16`]
/// with the channel-deinterleave step: callers that need the
/// big-endian decode without re-interleave should use
/// `pcm_decode_16be_to_i16` directly on the L and R halves.
pub fn pcm_deinterleave_stereo_16be(src: &[u8], dst: &mut [i16]) -> Result<()> {
    if src.len() % 4 != 0 {
        return Err(CinepakError::invalid(format!(
            "FILM PCM: stereo 16-bit BE chunk length must be a multiple of 4, got {}",
            src.len(),
        )));
    }
    let samples = src.len() / 2;
    if dst.len() != samples {
        return Err(CinepakError::invalid(format!(
            "FILM PCM: stereo 16-bit deinterleave length mismatch (src {} bytes ⇒ {} samples != dst {})",
            src.len(),
            samples,
            dst.len(),
        )));
    }
    let per_channel_samples = samples / 2;
    let per_channel_bytes = per_channel_samples * 2;
    let (left_bytes, right_bytes) = src.split_at(per_channel_bytes);
    for i in 0..per_channel_samples {
        let l = i16::from_be_bytes([left_bytes[2 * i], left_bytes[2 * i + 1]]);
        let r = i16::from_be_bytes([right_bytes[2 * i], right_bytes[2 * i + 1]]);
        dst[2 * i] = l;
        dst[2 * i + 1] = r;
    }
    Ok(())
}

impl FilmAudioFormat {
    /// One-shot convenience: decode a single FILM audio chunk's PCM
    /// payload to a `Vec<i16>` of host-endian, two's-complement,
    /// channel-interleaved samples ready for a standard PCM sink.
    ///
    /// Dispatches per the [`LinearPcm`] discriminator:
    ///
    /// | `bits_per_sample` | `channels` | Pipeline                                           |
    /// | ----------------- | ---------- | -------------------------------------------------- |
    /// | 8                 | 1          | [`pcm_decode_8bit`] then sign-extend each `i8`     |
    /// | 8                 | 2          | [`pcm_decode_8bit`] then sign-extend then re-interleave |
    /// | 16                | 1          | [`pcm_decode_16be_to_i16`]                         |
    /// | 16                | 2          | [`pcm_deinterleave_stereo_16be`]                   |
    ///
    /// Returns `None` for:
    ///
    /// - non-`LinearPcm` discriminators (callers route ADX ADPCM /
    ///   Unknown elsewhere),
    /// - unsupported `(bits_per_sample, channels)` combinations
    ///   (only the four cells above are documented in the wiki),
    /// - source-length / channel-count mismatches that would trip
    ///   the underlying helpers' size invariants.
    ///
    /// The 16-bit path expands each output `i8` to `i16` via the
    /// standard `i8 as i16` sign-extension; the resulting samples sit
    /// in the bottom 8 bits of an `i16` (no scaling to the full
    /// 16-bit range — that's a remixing decision left to the caller).
    ///
    /// [`LinearPcm`]: FilmAudioFormat::LinearPcm
    pub fn decode_chunk_to_i16(&self, src: &[u8]) -> Option<Vec<i16>> {
        let Self::LinearPcm {
            channels,
            bits_per_sample,
            sign_convention,
            ..
        } = *self
        else {
            return None;
        };
        match (bits_per_sample, channels) {
            (8, 1) => {
                let mut tmp = vec![0i8; src.len()];
                pcm_decode_8bit(src, sign_convention, &mut tmp).ok()?;
                Some(tmp.into_iter().map(|s| s as i16).collect())
            }
            (8, 2) => {
                if src.len() % 2 != 0 {
                    return None;
                }
                let mut decoded = vec![0i8; src.len()];
                pcm_decode_8bit(src, sign_convention, &mut decoded).ok()?;
                let per_channel = decoded.len() / 2;
                let mut out = vec![0i16; decoded.len()];
                for i in 0..per_channel {
                    out[2 * i] = decoded[i] as i16;
                    out[2 * i + 1] = decoded[per_channel + i] as i16;
                }
                Some(out)
            }
            (16, 1) => {
                if src.len() % 2 != 0 {
                    return None;
                }
                let mut out = vec![0i16; src.len() / 2];
                pcm_decode_16be_to_i16(src, &mut out).ok()?;
                Some(out)
            }
            (16, 2) => {
                if src.len() % 4 != 0 {
                    return None;
                }
                let mut out = vec![0i16; src.len() / 2];
                pcm_deinterleave_stereo_16be(src, &mut out).ok()?;
                Some(out)
            }
            _ => None,
        }
    }
}

/// Map a FILM version field to its documented PCM sign convention.
/// ASCII version (any printable `[0-9a-zA-Z.]` four-byte stamp) →
/// Saturn → twos-complement (wiki line 151). NULL or any non-ASCII
/// version → early Sega CD / 3DO → sign/magnitude (wiki line 162 + 224).
fn pcm_sign_convention_for(version: &[u8; 4]) -> PcmSignConvention {
    let ascii = version.iter().all(|b| {
        let c = *b;
        c.is_ascii_alphanumeric() || c == b'.'
    });
    if ascii {
        PcmSignConvention::TwosComplement
    } else {
        PcmSignConvention::SignMagnitude
    }
}

/// Size of the STAB chunk's fixed header on the wire, in bytes:
/// signature (4) + chunk length (4) + base_frequency (4) +
/// num_entries (4). The per-record sample table starts at this
/// offset. Reference: `Sega_FILM.wiki` lines 84-91
/// (`docs/video/cinepak/reference/wiki/Sega_FILM.wiki`).
pub const STAB_HEADER_SIZE: usize = 16;

/// Sample Table (`STAB`) header (excluding the per-sample records).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StabHeader {
    /// Sample-rate base for `sample_info_1` timestamps, in Hz.
    pub base_frequency: u32,
    /// Number of 16-byte sample records that follow.
    pub num_entries: u32,
}

impl StabHeader {
    /// Parse a STAB chunk's fixed 16-byte header from `chunk` and
    /// return the parsed [`StabHeader`] paired with the records-only
    /// byte slice that follows it — exactly the slice
    /// [`Samples::new`] expects.
    ///
    /// `chunk` is the STAB chunk starting at its `'STAB'` signature:
    /// equivalently, the suffix of a FILM header buffer at offset
    /// `FILM_HEADER_MIN_SIZE + fdsc_len`. The layout (per
    /// `Sega_FILM.wiki` lines 84-91,
    /// `docs/video/cinepak/reference/wiki/Sega_FILM.wiki`) is:
    ///
    /// - bytes 0-3   `'STAB'` chunk signature
    /// - bytes 4-7   length of STAB chunk (validation only — see below)
    /// - bytes 8-11  framerate base frequency in Hz
    /// - bytes 12-15 number of entries in the sample table
    /// - bytes 16-n  sample table (`num_entries * 16` bytes)
    ///
    /// This is the container-side bridge that hands the records slice
    /// straight to the r261 [`Samples`] walker without round-tripping
    /// through [`FilmDemuxer::parse`] (which allocates a
    /// `Vec<SampleRecord>`). A caller that only wants to walk the
    /// typed sample stream — a partial-header streamer, a validator,
    /// a demuxer sharing STAB parsing with a sister format — can do
    /// `let (hdr, recs) = StabHeader::parse_chunk(chunk)?;` then
    /// `Samples::new(recs)?`.
    ///
    /// The returned slice is bounded to exactly `num_entries * 16`
    /// bytes; any trailing bytes in `chunk` beyond the declared record
    /// table are not returned. The STAB `length` field at bytes 4-7 is
    /// **not** used for offset arithmetic — `Sega_FILM.wiki` line 92
    /// records that some titles (e.g. Burning Rangers, version `'1.09'`)
    /// omit the first 16 bytes from that field — so it is read only to
    /// be carried forward as a structural sanity value; correctness
    /// relies on `num_entries` alone.
    ///
    /// # Errors
    ///
    /// - the chunk is shorter than [`STAB_HEADER_SIZE`];
    /// - bytes 0-3 are not the `'STAB'` signature;
    /// - the declared `num_entries * 16` records overrun `chunk`.
    pub fn parse_chunk(chunk: &[u8]) -> Result<(Self, &[u8])> {
        if chunk.len() < STAB_HEADER_SIZE {
            return Err(CinepakError::invalid(format!(
                "STAB: chunk header truncated ({} < {STAB_HEADER_SIZE})",
                chunk.len()
            )));
        }
        if &chunk[0..4] != STAB_SIGNATURE {
            return Err(CinepakError::invalid(format!(
                "STAB: expected 'STAB' signature, got {:?}",
                &chunk[0..4]
            )));
        }
        // bytes 4-7: chunk length — validation/sanity only, not used
        // for offset arithmetic (Sega_FILM.wiki line 92).
        let _chunk_length = u32::from_be_bytes(chunk[4..8].try_into().unwrap());
        let base_frequency = u32::from_be_bytes(chunk[8..12].try_into().unwrap());
        let num_entries = u32::from_be_bytes(chunk[12..16].try_into().unwrap());
        let needed = (num_entries as usize)
            .checked_mul(SAMPLE_RECORD_SIZE)
            .ok_or_else(|| {
                CinepakError::invalid(format!(
                    "STAB: num_entries {num_entries} overflows record-table size"
                ))
            })?;
        let records_end = STAB_HEADER_SIZE
            .checked_add(needed)
            .ok_or_else(|| CinepakError::invalid("STAB: record-table extent overflows usize"))?;
        if chunk.len() < records_end {
            return Err(CinepakError::invalid(format!(
                "STAB: sample records truncated ({} needed, {} available)",
                needed,
                chunk.len() - STAB_HEADER_SIZE
            )));
        }
        let header = StabHeader {
            base_frequency,
            num_entries,
        };
        Ok((header, &chunk[STAB_HEADER_SIZE..records_end]))
    }
}

/// One row of the STAB sample table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleRecord {
    /// Offset of the sample bytes from the start of the sample-data
    /// block (file offset `header_length`).
    pub sample_offset: u32,
    /// Length of the sample in bytes (may exceed the codec frame
    /// length by 8 bytes per spec §2.7's deviation).
    pub sample_length: u32,
    /// Type / keyframe / timestamp discriminator. Special value
    /// `0xFFFFFFFF` denotes audio.
    pub sample_info_1: u32,
    /// Tick count to next frame (video) or `1` (audio).
    pub sample_info_2: u32,
}

impl SampleRecord {
    /// `0xFFFFFFFF` in `sample_info_1` denotes an audio sample.
    pub fn is_audio(&self) -> bool {
        self.sample_info_1 == 0xFFFF_FFFF
    }

    /// For video records: top bit of `sample_info_1` clear ⇒ keyframe.
    pub fn is_keyframe(&self) -> bool {
        !self.is_audio() && (self.sample_info_1 & 0x8000_0000) == 0
    }

    /// For video records: lower 31 bits of `sample_info_1` are the
    /// absolute timestamp in clock ticks of `STAB.base_frequency`.
    pub fn timestamp_ticks(&self) -> Option<u32> {
        if self.is_audio() {
            None
        } else {
            Some(self.sample_info_1 & 0x7FFF_FFFF)
        }
    }

    /// For video records: `sample_info_2` is the number of
    /// `STAB.base_frequency` ticks until the next frame is rendered
    /// (per `Sega_FILM.wiki` line 116). Returns `None` for audio
    /// samples — that field is always `1` for audio and carries no
    /// inter-frame-gap meaning.
    pub fn next_frame_ticks(&self) -> Option<u32> {
        if self.is_audio() {
            None
        } else {
            Some(self.sample_info_2)
        }
    }

    /// Returns `true` for an audio sample whose `sample_info_2` is
    /// exactly `1` per `Sega_FILM.wiki` line 116 ("For an audio chunk,
    /// sample info 1 is all ones and sample info 2 is always 1"). A
    /// non-1 `sample_info_2` on an audio record means the file is
    /// malformed (the wiki specifies the value verbatim); we surface it
    /// rather than silently accepting since downstream playback engines
    /// rely on the field being a constant.
    ///
    /// Returns `false` for video records — they aren't audio at all,
    /// and `sample_info_2` carries the video-only `next_frame_ticks`
    /// meaning, not the audio `1` constant.
    pub fn is_well_formed_audio(&self) -> bool {
        self.is_audio() && self.sample_info_2 == 1
    }
}

/// Size of one STAB sample record on the wire, in bytes.
pub const SAMPLE_RECORD_SIZE: usize = 16;

/// Classified kind of a single STAB sample record.
///
/// Derived from [`SampleRecord::is_audio`] +
/// [`SampleRecord::is_keyframe`] per `Sega_FILM.wiki` lines 102-116
/// (`docs/video/cinepak/reference/wiki/Sega_FILM.wiki`):
///
/// - `sample_info_1 == 0xFFFFFFFF` ⇒ audio sample (codec data routed
///   to the audio path; not Cinepak).
/// - top bit of `sample_info_1` clear (and not audio) ⇒ video
///   **keyframe** ("intra" / "I-frame"). Lower 31 bits are the
///   absolute timestamp in `STAB.base_frequency` ticks.
/// - top bit of `sample_info_1` set (and not audio) ⇒ video **inter**
///   frame (predicted from the previously-reconstructed frame).
///   Lower 31 bits are the timestamp.
///
/// Carries the per-kind metadata already classified so callers can
/// pattern-match without re-walking the bit fields of
/// `sample_info_1` / `sample_info_2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    /// Audio sample (`sample_info_1 == 0xFFFFFFFF`).
    ///
    /// `well_formed` carries the result of
    /// [`SampleRecord::is_well_formed_audio`] — `true` iff
    /// `sample_info_2 == 1` per `Sega_FILM.wiki` line 116. A `false`
    /// here flags a malformed audio interleave that downstream PCM
    /// decoders should refuse rather than silently process.
    Audio { well_formed: bool },
    /// Video keyframe (intra-coded). `timestamp_ticks` is the
    /// absolute frame timestamp in `STAB.base_frequency` ticks;
    /// `next_frame_ticks` is the tick count until the next frame
    /// renders (`sample_info_2`).
    VideoKeyframe {
        timestamp_ticks: u32,
        next_frame_ticks: u32,
    },
    /// Video inter frame (predicted from the previous frame). Same
    /// fields as [`SampleKind::VideoKeyframe`].
    VideoInter {
        timestamp_ticks: u32,
        next_frame_ticks: u32,
    },
}

impl SampleKind {
    /// Classify a raw [`SampleRecord`] into the typed enum.
    pub fn from_record(rec: SampleRecord) -> Self {
        if rec.is_audio() {
            SampleKind::Audio {
                well_formed: rec.is_well_formed_audio(),
            }
        } else {
            // Lower 31 bits = timestamp, top bit = inter flag.
            let ts = rec.sample_info_1 & 0x7FFF_FFFF;
            let next = rec.sample_info_2;
            if (rec.sample_info_1 & 0x8000_0000) == 0 {
                SampleKind::VideoKeyframe {
                    timestamp_ticks: ts,
                    next_frame_ticks: next,
                }
            } else {
                SampleKind::VideoInter {
                    timestamp_ticks: ts,
                    next_frame_ticks: next,
                }
            }
        }
    }

    /// `true` for [`SampleKind::Audio`].
    pub fn is_audio(&self) -> bool {
        matches!(self, SampleKind::Audio { .. })
    }

    /// `true` for [`SampleKind::VideoKeyframe`].
    pub fn is_keyframe(&self) -> bool {
        matches!(self, SampleKind::VideoKeyframe { .. })
    }

    /// `true` for [`SampleKind::VideoKeyframe`] or
    /// [`SampleKind::VideoInter`].
    pub fn is_video(&self) -> bool {
        matches!(
            self,
            SampleKind::VideoKeyframe { .. } | SampleKind::VideoInter { .. }
        )
    }
}

/// A single STAB record yielded by [`Samples`], carrying the raw
/// 16-byte [`SampleRecord`] alongside its already-classified
/// [`SampleKind`] and 0-based index within the record table.
///
/// Reference: `Sega_FILM.wiki` lines 84-116
/// (`docs/video/cinepak/reference/wiki/Sega_FILM.wiki`) — STAB chunk
/// layout and `sample_info_1` / `sample_info_2` semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleRecordEntry {
    /// 0-based record index within the STAB sample table.
    pub index: u32,
    /// Raw 16-byte sample record, untouched from the wire.
    pub record: SampleRecord,
    /// Classified kind (audio / video keyframe / video inter) with
    /// per-kind metadata extracted (timestamp + next-frame ticks for
    /// video; well-formedness for audio).
    pub kind: SampleKind,
}

/// Iterator over the per-record contents of a STAB sample-record
/// byte slice.
///
/// Wire-format reference: `Sega_FILM.wiki` lines 84-116
/// (`docs/video/cinepak/reference/wiki/Sega_FILM.wiki`). The STAB
/// chunk lays out a 16-byte header (signature + chunk length +
/// `base_frequency` + `num_entries`) followed by `num_entries`
/// 16-byte sample records of `(sample_offset, sample_length,
/// sample_info_1, sample_info_2)` big-endian `u32` fields.
///
/// [`Samples::new`] takes the records-only byte slice — i.e. the
/// portion of the FILM header **after** the STAB 16-byte
/// header — and yields one [`SampleRecordEntry`] per 16-byte row in
/// wire order. Each yield re-decodes the row's four big-endian
/// `u32`s into a [`SampleRecord`] and classifies it via
/// [`SampleKind::from_record`].
///
/// The iterator is **read-only** and **content-agnostic**: it
/// walks the records byte-for-byte without touching the FILM /
/// FDSC envelopes around it. Callers who already have the records
/// region in hand — typically because they parsed the STAB header
/// themselves, or because they're streaming a partial header
/// without buffering the full FILM file — can drive a typed
/// per-sample loop without round-tripping through
/// [`FilmDemuxer::parse`].
///
/// ### Composition with the existing typed-iterator family
///
/// Mirrors the strip-side typed walkers: r240 [`crate::header::FrameStrips`]
/// (frame → strips), r243 [`crate::codebook::StripChunks`]
/// (strip → chunks), r246 / r250 / r253 (per-MB walks of
/// `0x3200` / `0x3000` / `0x3100`), r256
/// [`crate::codebook::CodebookEntries`] (codebook chunk → per-slot
/// entries). This iterator is the container-side analogue: STAB
/// records → typed sample stream. With it the read-only typed
/// surface now covers every Cinepak wire layer from FILM
/// container-table down to per-codebook-slot.
///
/// ### Error semantics
///
/// [`Samples::new`] rejects record byte slices whose length is not
/// a multiple of [`SAMPLE_RECORD_SIZE`] up front (a well-formed STAB
/// always has length `num_entries * 16`). No per-yield errors are
/// possible after that — every 16-byte record decodes to a valid
/// [`SampleRecord`] + [`SampleKind`] by construction.
#[derive(Clone, Debug)]
pub struct Samples<'a> {
    /// Records byte slice. The iterator only reads within this slice.
    records: &'a [u8],
    /// Byte offset of the next record header within `records`.
    cursor: usize,
    /// 0-based index of the next record to yield.
    next_index: u32,
}

impl<'a> Samples<'a> {
    /// Build a sample iterator over the STAB records byte slice
    /// `records`.
    ///
    /// `records` is the `num_entries * 16` bytes that follow the
    /// 16-byte STAB chunk header — equivalently, the suffix of a
    /// FILM header buffer starting at offset
    /// `FILM_HEADER_MIN_SIZE + fdsc_len + 16`.
    ///
    /// Returns `Err` iff `records.len() % SAMPLE_RECORD_SIZE != 0`
    /// (a STAB record table cannot have a partial row).
    pub fn new(records: &'a [u8]) -> Result<Self> {
        if records.len() % SAMPLE_RECORD_SIZE != 0 {
            return Err(CinepakError::invalid(format!(
                "STAB sample-records slice length {} not a multiple of {SAMPLE_RECORD_SIZE}",
                records.len()
            )));
        }
        Ok(Self {
            records,
            cursor: 0,
            next_index: 0,
        })
    }

    /// Number of records the iterator has yet to yield.
    pub fn remaining(&self) -> u32 {
        ((self.records.len() - self.cursor) / SAMPLE_RECORD_SIZE) as u32
    }

    /// Byte offset of the next record-header read within the records
    /// slice. Useful for cross-referencing with a higher-level FILM
    /// walker.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 0-based index of the next record to yield.
    pub fn next_index(&self) -> u32 {
        self.next_index
    }

    /// Underlying records byte slice (unchanged from construction).
    pub fn records_bytes(&self) -> &'a [u8] {
        self.records
    }
}

impl<'a> Iterator for Samples<'a> {
    type Item = SampleRecordEntry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor + SAMPLE_RECORD_SIZE > self.records.len() {
            return None;
        }
        let o = self.cursor;
        // u32::from_be_bytes on 4-byte slices that are guaranteed to
        // be in-range — the construction-time length check + this
        // cursor-bound `if` together imply each slice is exactly 4 B.
        let sample_offset = u32::from_be_bytes(self.records[o..o + 4].try_into().unwrap());
        let sample_length = u32::from_be_bytes(self.records[o + 4..o + 8].try_into().unwrap());
        let sample_info_1 = u32::from_be_bytes(self.records[o + 8..o + 12].try_into().unwrap());
        let sample_info_2 = u32::from_be_bytes(self.records[o + 12..o + 16].try_into().unwrap());
        let record = SampleRecord {
            sample_offset,
            sample_length,
            sample_info_1,
            sample_info_2,
        };
        let kind = SampleKind::from_record(record);
        let entry = SampleRecordEntry {
            index: self.next_index,
            record,
            kind,
        };
        self.cursor += SAMPLE_RECORD_SIZE;
        self.next_index += 1;
        Some(entry)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let rem = self.remaining() as usize;
        (rem, Some(rem))
    }
}

impl<'a> ExactSizeIterator for Samples<'a> {}

impl<'a> std::iter::FusedIterator for Samples<'a> {}

/// A parsed FILM container's full header (FILM + FDSC + STAB and the
/// list of sample records). The sample bytes themselves are not read;
/// callers consume them by reading `sample_length` bytes from the file
/// at offset `header_length + sample.sample_offset`.
#[derive(Clone, Debug)]
pub struct FilmDemuxer {
    pub film_header: FilmHeader,
    pub fdsc: Fdsc,
    pub stab_header: StabHeader,
    pub samples: Vec<SampleRecord>,
}

impl FilmDemuxer {
    /// Parse a FILM header (FILM + FDSC + STAB + sample records) from
    /// `bytes`. The buffer must contain at least the first
    /// `header_length` bytes of the file; trailing bytes are ignored.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let film_header = FilmHeader::parse(bytes)?;
        let mut p = FILM_HEADER_MIN_SIZE;
        let (fdsc, fdsc_len) = Fdsc::parse(&bytes[p..])?;
        p += fdsc_len as usize;

        if bytes.len() < p + 16 {
            return Err(CinepakError::invalid(
                "FILM: STAB chunk header truncated after FDSC",
            ));
        }
        if &bytes[p..p + 4] != STAB_SIGNATURE {
            return Err(CinepakError::invalid(format!(
                "FILM: expected 'STAB' signature after FDSC, got {:?}",
                &bytes[p..p + 4]
            )));
        }
        let stab_chunk_length = u32::from_be_bytes(bytes[p + 4..p + 8].try_into().unwrap());
        let base_frequency = u32::from_be_bytes(bytes[p + 8..p + 12].try_into().unwrap());
        let num_entries = u32::from_be_bytes(bytes[p + 12..p + 16].try_into().unwrap());
        // Sample records start at p + 16. STAB chunk length sometimes
        // doesn't include the first 16 bytes per `Sega_FILM.wiki` line
        // 92 — we don't rely on that field for offsets, only for
        // structural validation.
        let _ = stab_chunk_length;
        let records_start = p + 16;
        let needed = (num_entries as usize) * 16;
        if bytes.len() < records_start + needed {
            return Err(CinepakError::invalid(format!(
                "FILM: STAB sample records truncated ({} needed, {} available)",
                needed,
                bytes.len() - records_start
            )));
        }
        let mut samples = Vec::with_capacity(num_entries as usize);
        for i in 0..(num_entries as usize) {
            let off = records_start + i * 16;
            let sample_offset = u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap());
            let sample_length = u32::from_be_bytes(bytes[off + 4..off + 8].try_into().unwrap());
            let sample_info_1 = u32::from_be_bytes(bytes[off + 8..off + 12].try_into().unwrap());
            let sample_info_2 = u32::from_be_bytes(bytes[off + 12..off + 16].try_into().unwrap());
            samples.push(SampleRecord {
                sample_offset,
                sample_length,
                sample_info_1,
                sample_info_2,
            });
        }

        Ok(Self {
            film_header,
            fdsc,
            stab_header: StabHeader {
                base_frequency,
                num_entries,
            },
            samples,
        })
    }

    /// Returns the file offset (in bytes from the start of the file)
    /// of the codec data for sample `idx`.
    pub fn sample_file_offset(&self, idx: usize) -> Option<u64> {
        let s = self.samples.get(idx)?;
        Some(u64::from(self.film_header.header_length) + u64::from(s.sample_offset))
    }

    /// Returns just the video samples, in their original sample-table
    /// order. Useful when the caller doesn't care about audio
    /// interleaving.
    pub fn video_samples(&self) -> impl Iterator<Item = (usize, &SampleRecord)> {
        self.samples
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_audio())
    }

    /// Mirror of [`Self::video_samples`] for audio: returns just the
    /// records whose `sample_info_1 == 0xFFFFFFFF` per
    /// `Sega_FILM.wiki` line 104. Audio codec decoding is out of
    /// scope for this crate, but the iterator is useful for callers
    /// that need to skip over audio in the interleaved sample stream
    /// to pull only the audio bytes (e.g. routing them to a separate
    /// audio decoder).
    pub fn audio_samples(&self) -> impl Iterator<Item = (usize, &SampleRecord)> {
        self.samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_audio())
    }

    /// Classify the audio stream's format using both the FDSC's audio
    /// fields and the [`FilmHeader::version`] (required because the
    /// PCM sign convention depends on the platform). Convenience
    /// wrapper over [`Fdsc::audio_format`] that threads the version
    /// automatically. See [`FilmAudioFormat`] for the discriminator
    /// taxonomy.
    pub fn audio_format(&self) -> FilmAudioFormat {
        self.fdsc.audio_format(&self.film_header.version)
    }

    /// Total **audio** stream duration in seconds, computed from the
    /// sum of audio `sample_length` fields divided by the linear-PCM
    /// byte rate (see [`FilmAudioFormat::byte_rate_bps`]).
    ///
    /// Returns `None` when:
    ///
    /// - the file declares no audio (`has_audio() == false`),
    /// - the audio compression is not linear PCM (the byte-to-sample
    ///   mapping is codec-internal for ADX ADPCM),
    /// - any of `channels` / `bits_per_sample` / `sample_rate_hz`
    ///   would yield a zero byte rate,
    /// - or the file contains no audio sample records at all.
    ///
    /// This is independent of the video timeline; for the **video**
    /// duration use [`Self::duration_seconds`].
    pub fn audio_duration_seconds(&self) -> Option<f64> {
        let byte_rate = self.audio_format().byte_rate_bps()?;
        if byte_rate == 0 {
            return None;
        }
        let total_bytes: u64 = self
            .audio_samples()
            .map(|(_, s)| u64::from(s.sample_length))
            .sum();
        if total_bytes == 0 {
            return None;
        }
        // `byte_rate` is u64 (max product = 255 channels × 8 B/sample ×
        // 65535 Hz ≈ 1.3 × 10^8 — well within f64 exact integers).
        Some(total_bytes as f64 / byte_rate as f64)
    }

    /// Validate every audio sample record's `sample_info_2 == 1` per
    /// `Sega_FILM.wiki` line 116. Returns the index of the first
    /// audio record whose `sample_info_2` is **not** 1, or `None` if
    /// all audio records are well-formed (or there are no audio
    /// records at all).
    ///
    /// Use as a defensive validation pass after [`Self::parse`] — the
    /// wiki specifies the field's value verbatim, so a non-1 value
    /// indicates either (a) a malformed file or (b) a producer that
    /// silently broke the audio interleave contract; either case is
    /// worth surfacing rather than silently routing the audio bytes
    /// to a PCM decoder that assumes the constant.
    pub fn first_malformed_audio_sample(&self) -> Option<usize> {
        self.audio_samples()
            .find(|(_, s)| !s.is_well_formed_audio())
            .map(|(i, _)| i)
    }

    /// Returns just the video **keyframes**, in sample-table order.
    /// Per `Sega_FILM.wiki` line 104 a video record's
    /// `sample_info_1`-top-bit clear identifies a keyframe — the wiki
    /// notes this "is useful for seeking since it is a good idea to
    /// only jump to key frames when seeking through a file". This
    /// helper is the entry point most seek implementations want.
    pub fn keyframes(&self) -> impl Iterator<Item = (usize, &SampleRecord)> {
        self.samples
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_audio() && s.is_keyframe())
    }

    /// Find the video keyframe whose timestamp (in
    /// [`StabHeader::base_frequency`] ticks) is the **largest value
    /// less than or equal to** `target_ticks`. Returns the sample
    /// index + record, or `None` if no keyframe meets the criterion
    /// (i.e. `target_ticks < ts(first keyframe)`, including the
    /// no-keyframe case).
    ///
    /// This is the standard "snap-to-keyframe" seek primitive: the
    /// caller picks the playback time the user requested (in
    /// `base_frequency` ticks), this helper returns the keyframe
    /// they must restart decoding from, and the caller skips
    /// intermediate inter frames until reaching the target tick.
    ///
    /// The sample table is **not** assumed to be timestamp-sorted —
    /// callers in the wild sometimes interleave samples non-
    /// monotonically — so the scan is O(n) over the keyframes, not
    /// a binary search. With Cinepak's CD-era frame counts (a few
    /// thousand samples at most) the linear scan is well within
    /// budget for any conceivable use.
    pub fn seek_keyframe_for_tick(&self, target_ticks: u32) -> Option<(usize, &SampleRecord)> {
        let mut best: Option<(usize, &SampleRecord, u32)> = None;
        for (idx, s) in self.keyframes() {
            // `is_keyframe()` already excluded audio, so
            // `timestamp_ticks()` returns `Some` here.
            let ts = s.timestamp_ticks().unwrap_or(0);
            if ts <= target_ticks {
                match best {
                    None => best = Some((idx, s, ts)),
                    Some((_, _, bts)) if ts > bts => best = Some((idx, s, ts)),
                    _ => {}
                }
            }
        }
        best.map(|(idx, s, _)| (idx, s))
    }

    /// Total stream duration in `base_frequency` ticks: the
    /// highest video-sample timestamp + that sample's
    /// `next_frame_ticks()`. Returns `None` when the file contains
    /// no video samples.
    ///
    /// Per `Sega_FILM.wiki` line 116, a video sample's
    /// `sample_info_2` is the tick count until the next frame
    /// renders — so the natural duration of a video clip is
    /// `max(ts) + ticks_to_next` of that last frame.
    pub fn duration_ticks(&self) -> Option<u32> {
        let mut max_ts = None::<u32>;
        let mut max_end = None::<u32>;
        for (_, s) in self.video_samples() {
            let ts = s.timestamp_ticks().unwrap_or(0);
            let next = s.next_frame_ticks().unwrap_or(0);
            let end = ts.saturating_add(next);
            match max_ts {
                None => {
                    max_ts = Some(ts);
                    max_end = Some(end);
                }
                Some(prev_ts) if ts > prev_ts => {
                    max_ts = Some(ts);
                    max_end = Some(end);
                }
                _ => {}
            }
        }
        max_end
    }

    /// Total stream duration in seconds, computed as
    /// [`Self::duration_ticks`] / [`StabHeader::base_frequency`].
    /// Returns `None` if no video samples are present or the
    /// recorded `base_frequency` is zero (a degenerate file).
    pub fn duration_seconds(&self) -> Option<f64> {
        let ticks = self.duration_ticks()?;
        let base = self.stab_header.base_frequency;
        if base == 0 {
            None
        } else {
            Some(f64::from(ticks) / f64::from(base))
        }
    }

    /// Classify the Cinepak wire-format variant in use by this FILM
    /// file, per `Sega_FILM.wiki` "Strategy For Detecting FILM File
    /// Types" (lines 200–224). The classification is purely
    /// header-driven; the actual sample bytes are not inspected.
    ///
    /// Decision rules (in order):
    ///
    /// 1. FOURCC `'sega'` or `'Seg4'` → [`CinepakVariant::OutOfScope`]
    ///    (Cinepak-for-Sega is a different codec).
    /// 2. FOURCC `'cvid'` + ASCII version `'1.0X'` or higher → all
    ///    Saturn `.cpk` files (per wiki line 211, "load as standard
    ///    FILM file, be sure to feed CVID data through Cinepak
    ///    decoder that can handle it") → [`CinepakVariant::DeviantSaturn`].
    /// 3. FOURCC `'cvid'` + NULL version field → Lemmings 3DO
    ///    (per wiki line 189) → [`CinepakVariant::DeviantLemmings3do`].
    /// 4. Anything else with `'cvid'` falls back to standard.
    ///
    /// Note: there is no standard non-Saturn FILM container in the
    /// wild — every `.cpk` file with a `'cvid'` codec uses some flavour
    /// of the deviant Saturn variant per `Sega_FILM.wiki` lines
    /// 125–127 ("The Cinepak data inside of a FILM file can not be
    /// decoded with a general purpose Cinepak decoding algorithm").
    /// Use [`crate::CinepakDecoder::decode_frame`] only when the
    /// frames come from an AVI / QuickTime container, not from FILM.
    pub fn variant(&self) -> CinepakVariant {
        match &self.fdsc.video_codec {
            b"sega" | b"Seg4" => CinepakVariant::OutOfScope,
            b"cvid" => {
                // NULL version = Lemmings 3DO per wiki line 189.
                if self.film_header.version == [0u8, 0, 0, 0] {
                    CinepakVariant::DeviantLemmings3do
                } else {
                    // ASCII '1.0X' or any other version = standard
                    // Saturn deviant per wiki line 211.
                    CinepakVariant::DeviantSaturn
                }
            }
            _ => CinepakVariant::OutOfScope,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid FILM header with one keyframe video
    /// sample referenced by the STAB. Verifies the parser walks the
    /// FILM + FDSC + STAB chain correctly.
    fn build_minimal_film() -> Vec<u8> {
        let mut out = Vec::new();
        // FILM header (16 B).
        out.extend_from_slice(b"FILM");
        // header_length = 16 + 32 (FDSC) + 16 (STAB hdr) + 16 (1 record) = 80
        out.extend_from_slice(&80u32.to_be_bytes());
        out.extend_from_slice(b"1.09");
        out.extend_from_slice(&0u32.to_be_bytes()); // reserved
                                                    // FDSC chunk (32 B).
        out.extend_from_slice(b"FDSC");
        out.extend_from_slice(&0x20u32.to_be_bytes()); // chunk_length
        out.extend_from_slice(b"cvid");
        out.extend_from_slice(&120u32.to_be_bytes()); // height
        out.extend_from_slice(&160u32.to_be_bytes()); // width
        out.push(24); // bpp
        out.push(0); // audio_channels
        out.push(0); // audio_bits
        out.push(0); // audio_compression
        out.extend_from_slice(&0u16.to_be_bytes()); // audio_sample_rate
        out.extend_from_slice(&[0u8; 6]); // reserved
                                          // STAB chunk: signature + length + base_frequency + num_entries + 1 record.
        out.extend_from_slice(b"STAB");
        out.extend_from_slice(&32u32.to_be_bytes()); // chunk_length (16 + 16)
        out.extend_from_slice(&10u32.to_be_bytes()); // base_frequency
        out.extend_from_slice(&1u32.to_be_bytes()); // num_entries
                                                    // Sample record: video keyframe, ts=0
        out.extend_from_slice(&0u32.to_be_bytes()); // sample_offset
        out.extend_from_slice(&500u32.to_be_bytes()); // sample_length
        out.extend_from_slice(&0u32.to_be_bytes()); // sample_info_1 (key, ts=0)
        out.extend_from_slice(&1u32.to_be_bytes()); // sample_info_2
        out
    }

    #[test]
    fn probe_recognises_signature() {
        assert!(probe_film(b"FILM\x00\x00\x00\x10"));
        assert!(!probe_film(b"RIFF"));
        assert!(!probe_film(b"FIL"));
    }

    #[test]
    fn parses_minimal_header() {
        let bytes = build_minimal_film();
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert_eq!(dem.film_header.header_length, 80);
        assert_eq!(&dem.film_header.version, b"1.09");
        assert!(dem.fdsc.is_standard_cinepak());
        assert_eq!(dem.fdsc.height, 120);
        assert_eq!(dem.fdsc.width, 160);
        assert_eq!(dem.fdsc.bits_per_pixel, 24);
        assert_eq!(dem.stab_header.num_entries, 1);
        assert_eq!(dem.samples.len(), 1);
        let s = &dem.samples[0];
        assert_eq!(s.sample_offset, 0);
        assert_eq!(s.sample_length, 500);
        assert!(!s.is_audio());
        assert!(s.is_keyframe());
        assert_eq!(s.timestamp_ticks(), Some(0));
        // First sample's file offset = header_length + 0.
        assert_eq!(dem.sample_file_offset(0), Some(80));
    }

    /// Spec §2.7 fixture `C3` STAB layout: 5 samples, sample 0 is
    /// keyframe, samples 1..4 are inter with ascending timestamps.
    #[test]
    fn keyframe_and_timestamp_classification() {
        // Sample 0: keyframe, ts=0.
        let s0 = SampleRecord {
            sample_offset: 0,
            sample_length: 5177,
            sample_info_1: 0x00000000,
            sample_info_2: 1,
        };
        assert!(s0.is_keyframe());
        assert_eq!(s0.timestamp_ticks(), Some(0));
        // Sample 1: inter, ts=1.
        let s1 = SampleRecord {
            sample_offset: 0x1439,
            sample_length: 2590,
            sample_info_1: 0x80000001,
            sample_info_2: 1,
        };
        assert!(!s1.is_keyframe());
        assert_eq!(s1.timestamp_ticks(), Some(1));
        // Audio sentinel.
        let sa = SampleRecord {
            sample_offset: 0,
            sample_length: 0,
            sample_info_1: 0xFFFF_FFFF,
            sample_info_2: 1,
        };
        assert!(sa.is_audio());
        assert_eq!(sa.timestamp_ticks(), None);
        assert!(!sa.is_keyframe());
    }

    #[test]
    fn rejects_wrong_signature() {
        let mut bytes = build_minimal_film();
        bytes[0] = b'X';
        assert!(FilmDemuxer::parse(&bytes).is_err());
    }

    #[test]
    fn rejects_truncated() {
        let bytes = build_minimal_film();
        // Truncate to less than 16 bytes.
        assert!(FilmDemuxer::parse(&bytes[..10]).is_err());
        // Truncate to 16 bytes — FDSC parse will fail.
        assert!(FilmDemuxer::parse(&bytes[..16]).is_err());
    }

    #[test]
    fn rejects_non_cvid_codec() {
        let mut bytes = build_minimal_film();
        // Replace 'cvid' at FDSC video_codec offset.
        bytes[16 + 8..16 + 12].copy_from_slice(b"sega");
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert!(!dem.fdsc.is_standard_cinepak());
    }

    /// Abbreviated 20-byte FDSC layout (early Sega CD).
    #[test]
    fn parses_abbreviated_fdsc() {
        let mut out = Vec::new();
        // FILM header.
        out.extend_from_slice(b"FILM");
        out.extend_from_slice(&((16 + 0x14 + 16) as u32).to_be_bytes());
        out.extend_from_slice(b"\x00\x02\x00\x00"); // early-variant version
        out.extend_from_slice(&0u32.to_be_bytes());
        // FDSC abbreviated (20 B).
        out.extend_from_slice(b"FDSC");
        out.extend_from_slice(&0x14u32.to_be_bytes());
        out.extend_from_slice(b"cvid");
        out.extend_from_slice(&80u32.to_be_bytes());
        out.extend_from_slice(&80u32.to_be_bytes());
        // STAB chunk (16 B header, 0 records).
        out.extend_from_slice(b"STAB");
        out.extend_from_slice(&16u32.to_be_bytes());
        out.extend_from_slice(&30u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());

        let dem = FilmDemuxer::parse(&out).unwrap();
        assert_eq!(dem.fdsc.width, 80);
        assert_eq!(dem.fdsc.height, 80);
        assert_eq!(dem.fdsc.bits_per_pixel, 0); // not present in abbreviated form
        assert_eq!(dem.stab_header.num_entries, 0);
        assert_eq!(dem.samples.len(), 0);
    }

    #[test]
    fn video_samples_skips_audio() {
        let mut out = build_minimal_film();
        // Patch num_entries to 2 and append an audio record.
        // num_entries lives at offset 16 (FILM) + 32 (FDSC) + 12 = 60.
        let num_off = 16 + 32 + 12;
        out[num_off..num_off + 4].copy_from_slice(&2u32.to_be_bytes());
        // Append second record (audio).
        out.extend_from_slice(&500u32.to_be_bytes()); // offset
        out.extend_from_slice(&100u32.to_be_bytes()); // length
        out.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // audio sentinel
        out.extend_from_slice(&1u32.to_be_bytes()); // sample_info_2
                                                    // Patch header_length to include the extra record (was 80).
        out[4..8].copy_from_slice(&96u32.to_be_bytes());
        // Patch STAB chunk_length too (not used for offsets but kept consistent).
        let stab_len_off = 16 + 32 + 4;
        out[stab_len_off..stab_len_off + 4].copy_from_slice(&((16 + 32) as u32).to_be_bytes());
        let dem = FilmDemuxer::parse(&out).unwrap();
        let video: Vec<_> = dem.video_samples().collect();
        assert_eq!(video.len(), 1);
        assert_eq!(video[0].0, 0);
    }

    // ---- r187: seek-friendly helpers -----------------------------------

    /// Build a FILM file with `records` raw 16-byte STAB rows. The
    /// first record's file-offset starts at 0; subsequent records are
    /// appended in order. `header_length` is auto-computed.
    fn build_film_with_records(records: &[[u32; 4]]) -> Vec<u8> {
        let mut out = Vec::new();
        let header_length: u32 = 16 + 32 + 16 + 16 * records.len() as u32;
        // FILM header.
        out.extend_from_slice(b"FILM");
        out.extend_from_slice(&header_length.to_be_bytes());
        out.extend_from_slice(b"1.09");
        out.extend_from_slice(&0u32.to_be_bytes());
        // FDSC chunk (32 B).
        out.extend_from_slice(b"FDSC");
        out.extend_from_slice(&0x20u32.to_be_bytes());
        out.extend_from_slice(b"cvid");
        out.extend_from_slice(&64u32.to_be_bytes()); // height
        out.extend_from_slice(&64u32.to_be_bytes()); // width
        out.extend_from_slice(&[24u8, 0, 0, 0]);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&[0u8; 6]);
        // STAB chunk header.
        out.extend_from_slice(b"STAB");
        let stab_chunk_length: u32 = 16 + 16 * records.len() as u32;
        out.extend_from_slice(&stab_chunk_length.to_be_bytes());
        out.extend_from_slice(&600u32.to_be_bytes()); // base_frequency = 600 Hz
        out.extend_from_slice(&(records.len() as u32).to_be_bytes());
        for r in records {
            out.extend_from_slice(&r[0].to_be_bytes());
            out.extend_from_slice(&r[1].to_be_bytes());
            out.extend_from_slice(&r[2].to_be_bytes());
            out.extend_from_slice(&r[3].to_be_bytes());
        }
        out
    }

    #[test]
    fn next_frame_ticks_is_video_only() {
        let video = SampleRecord {
            sample_offset: 0,
            sample_length: 5177,
            sample_info_1: 0x00000040, // keyframe, ts=64
            sample_info_2: 20,
        };
        assert_eq!(video.next_frame_ticks(), Some(20));
        let audio = SampleRecord {
            sample_offset: 0,
            sample_length: 1024,
            sample_info_1: 0xFFFF_FFFF,
            sample_info_2: 1,
        };
        assert_eq!(audio.next_frame_ticks(), None);
    }

    #[test]
    fn audio_samples_iterates_only_audio() {
        // 4 records: video(K, ts=0), audio, video(I, ts=20), audio.
        let bytes = build_film_with_records(&[
            [0, 500, 0x00000000, 20],
            [500, 100, 0xFFFF_FFFF, 1],
            [600, 300, 0x80000014, 20],
            [900, 100, 0xFFFF_FFFF, 1],
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        let audio: Vec<_> = dem.audio_samples().collect();
        assert_eq!(audio.len(), 2);
        // Sample-table indices 1 and 3 are audio.
        assert_eq!(audio[0].0, 1);
        assert_eq!(audio[1].0, 3);
    }

    #[test]
    fn keyframes_iterates_only_video_keyframes() {
        // K=ts0, I=ts20, K=ts40, I=ts60.
        let bytes = build_film_with_records(&[
            [0, 100, 0x00000000, 20],   // V key ts=0
            [100, 100, 0x80000014, 20], // V inter ts=20
            [200, 100, 0x00000028, 20], // V key ts=40
            [300, 100, 0x8000003c, 20], // V inter ts=60 (note: 0x3c = 60)
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        let kfs: Vec<_> = dem.keyframes().collect();
        assert_eq!(kfs.len(), 2);
        assert_eq!(kfs[0].0, 0);
        assert_eq!(kfs[0].1.timestamp_ticks(), Some(0));
        assert_eq!(kfs[1].0, 2);
        assert_eq!(kfs[1].1.timestamp_ticks(), Some(40));
    }

    #[test]
    fn keyframes_excludes_audio_with_top_bit_set() {
        // An audio record's `sample_info_1 == 0xFFFFFFFF` has the
        // top bit set; without the explicit audio filter on the
        // keyframe iterator, `!is_keyframe()` would already exclude
        // it — but defence-in-depth: the iterator should never yield
        // an audio record under any circumstances.
        let bytes = build_film_with_records(&[
            [0, 100, 0x00000000, 20],   // V key
            [100, 100, 0xFFFF_FFFF, 1], // audio
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        let kfs: Vec<_> = dem.keyframes().collect();
        assert_eq!(kfs.len(), 1);
        assert_eq!(kfs[0].0, 0);
    }

    #[test]
    fn seek_keyframe_for_tick_snaps_to_latest_at_or_below() {
        // K=0, I=20, K=40, I=60, K=80.
        let bytes = build_film_with_records(&[
            [0, 100, 0x00000000, 20],   // K, ts=0
            [100, 100, 0x80000014, 20], // I, ts=20
            [200, 100, 0x00000028, 20], // K, ts=40
            [300, 100, 0x8000003c, 20], // I, ts=60
            [400, 100, 0x00000050, 20], // K, ts=80
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        // Seek to before any keyframe: ts=0 is the floor, found.
        let (idx, s) = dem.seek_keyframe_for_tick(0).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(s.timestamp_ticks(), Some(0));
        // Seek between K@0 and K@40: snap back to K@0.
        let (idx, _) = dem.seek_keyframe_for_tick(35).unwrap();
        assert_eq!(idx, 0);
        // Seek exactly at K@40.
        let (idx, _) = dem.seek_keyframe_for_tick(40).unwrap();
        assert_eq!(idx, 2);
        // Seek between K@40 and K@80: snap to K@40.
        let (idx, _) = dem.seek_keyframe_for_tick(79).unwrap();
        assert_eq!(idx, 2);
        // Seek at K@80.
        let (idx, _) = dem.seek_keyframe_for_tick(80).unwrap();
        assert_eq!(idx, 4);
        // Seek past EOS: snap to last keyframe.
        let (idx, _) = dem.seek_keyframe_for_tick(10_000).unwrap();
        assert_eq!(idx, 4);
    }

    #[test]
    fn seek_keyframe_for_tick_returns_none_when_no_keyframes() {
        // All-inter sample table — pathological but expressible.
        let bytes = build_film_with_records(&[
            [0, 100, 0x80000000, 20],   // I, ts=0
            [100, 100, 0x80000014, 20], // I, ts=20
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert!(dem.seek_keyframe_for_tick(0).is_none());
        assert!(dem.seek_keyframe_for_tick(50).is_none());
    }

    #[test]
    fn seek_keyframe_for_tick_handles_unsorted_table() {
        // Wire order: K@40, then K@0 — snap should still return the
        // record whose ts is the largest ≤ target, regardless of
        // table order.
        let bytes = build_film_with_records(&[
            [0, 100, 0x00000028, 20],   // K, ts=40
            [100, 100, 0x00000000, 20], // K, ts=0
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        // Target 30: closest keyframe ≤ 30 is K@0, which is the
        // *second* table entry (idx=1).
        let (idx, _) = dem.seek_keyframe_for_tick(30).unwrap();
        assert_eq!(idx, 1);
        // Target 50: closest keyframe ≤ 50 is K@40, idx=0.
        let (idx, _) = dem.seek_keyframe_for_tick(50).unwrap();
        assert_eq!(idx, 0);
    }

    #[test]
    fn duration_ticks_uses_last_frame_plus_next_gap() {
        // 30 fps at 600 Hz base: 20 ticks per frame. 3 frames.
        let bytes = build_film_with_records(&[
            [0, 100, 0x00000000, 20],   // K, ts=0,  next=20
            [100, 100, 0x80000014, 20], // I, ts=20, next=20
            [200, 100, 0x80000028, 20], // I, ts=40, next=20
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        // Duration = ts(last) + next_frame_ticks(last) = 40 + 20 = 60.
        assert_eq!(dem.duration_ticks(), Some(60));
        // base_frequency = 600 Hz, so 60 ticks = 0.1 s.
        assert!((dem.duration_seconds().unwrap() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn duration_ticks_handles_variable_frame_gap() {
        // Wiki line 114 — Myst-style: 30 Hz base, gaps of 2/3 ticks.
        let bytes = build_film_with_records(&[
            [0, 100, 0x00000000, 2],   // K, ts=0,  next=2
            [100, 100, 0x80000002, 3], // I, ts=2,  next=3
            [200, 100, 0x80000005, 2], // I, ts=5,  next=2
            [300, 100, 0x80000007, 3], // I, ts=7,  next=3
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        // Last frame ts=7, next=3 → duration=10.
        assert_eq!(dem.duration_ticks(), Some(10));
    }

    #[test]
    fn duration_handles_no_video_or_zero_base() {
        // Audio-only — no video samples.
        let bytes =
            build_film_with_records(&[[0, 100, 0xFFFF_FFFF, 1], [100, 100, 0xFFFF_FFFF, 1]]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert_eq!(dem.duration_ticks(), None);
        assert_eq!(dem.duration_seconds(), None);
    }

    #[test]
    fn duration_seconds_returns_none_for_zero_base_frequency() {
        // Build a file then patch base_frequency to 0 — a degenerate
        // value the parser doesn't (currently) reject.
        let mut bytes = build_film_with_records(&[
            [0, 100, 0x00000000, 20], // K, ts=0, next=20
        ]);
        // base_frequency is at: 16 (FILM) + 32 (FDSC) + 8 (STAB sig+len)
        let base_off = 16 + 32 + 8;
        bytes[base_off..base_off + 4].copy_from_slice(&0u32.to_be_bytes());
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        // duration_ticks is still computable (it doesn't depend on base).
        assert_eq!(dem.duration_ticks(), Some(20));
        // duration_seconds short-circuits on zero base.
        assert_eq!(dem.duration_seconds(), None);
    }

    // ---- r221: audio-format classification ------------------------------

    /// Patch the FDSC audio-field bytes of a `build_minimal_film()`
    /// buffer in place. The FDSC starts at offset 16; per
    /// `Sega_FILM.wiki` lines 74–77 the audio fields are:
    ///   byte 21 = audio_channels
    ///   byte 22 = audio_bits
    ///   byte 23 = audio_compression
    ///   bytes 24–25 = audio_sample_rate (BE u16)
    fn patch_minimal_audio_fields(
        bytes: &mut [u8],
        channels: u8,
        bits: u8,
        compression: u8,
        sample_rate: u16,
    ) {
        let fdsc_off = 16;
        bytes[fdsc_off + 21] = channels;
        bytes[fdsc_off + 22] = bits;
        bytes[fdsc_off + 23] = compression;
        bytes[fdsc_off + 24..fdsc_off + 26].copy_from_slice(&sample_rate.to_be_bytes());
    }

    /// Patch the FILM version field (offset 8..12) of a
    /// `build_minimal_film()` buffer in place.
    fn patch_film_version(bytes: &mut [u8], version: &[u8; 4]) {
        bytes[8..12].copy_from_slice(version);
    }

    #[test]
    fn has_audio_zero_fields() {
        // Default `build_minimal_film()` builds an all-zero audio FDSC.
        let bytes = build_minimal_film();
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert!(!dem.fdsc.has_audio());
        assert_eq!(dem.audio_format(), FilmAudioFormat::None);
        // No audio ⇒ no byte rate ⇒ no audio duration.
        assert_eq!(dem.audio_format().byte_rate_bps(), None);
        assert_eq!(dem.audio_duration_seconds(), None);
        assert!(!dem.audio_format().is_linear_pcm());
    }

    #[test]
    fn has_audio_any_field_set() {
        // Setting any one of the four fields ⇒ has_audio() true.
        for (chan, bits, comp, rate) in [
            (1, 0, 0, 0u16),
            (0, 8, 0, 0),
            (0, 0, 2, 0),
            (0, 0, 0, 22050),
        ] {
            let mut bytes = build_minimal_film();
            patch_minimal_audio_fields(&mut bytes, chan, bits, comp, rate);
            let dem = FilmDemuxer::parse(&bytes).unwrap();
            assert!(
                dem.fdsc.has_audio(),
                "expected has_audio() for chan={chan} bits={bits} comp={comp} rate={rate}"
            );
        }
    }

    #[test]
    fn audio_format_saturn_stereo_16bit_pcm() {
        // Saturn ASCII version '1.07' with 16-bit stereo 44100 Hz PCM:
        // wiki line 151 (signed twos-complement) + line 153 (big-endian).
        let mut bytes = build_minimal_film();
        patch_film_version(&mut bytes, b"1.07");
        patch_minimal_audio_fields(&mut bytes, 2, 16, 0, 44100);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert_eq!(
            dem.audio_format(),
            FilmAudioFormat::LinearPcm {
                channels: 2,
                bits_per_sample: 16,
                sample_rate_hz: 44100,
                endianness: PcmEndianness::BigEndian,
                sign_convention: PcmSignConvention::TwosComplement,
            }
        );
        // byte rate = 2 ch × 2 B × 44100 Hz = 176_400 B/s.
        assert_eq!(dem.audio_format().byte_rate_bps(), Some(176_400));
        assert!(dem.audio_format().is_linear_pcm());
    }

    #[test]
    fn audio_format_saturn_8bit_pcm_endianness_not_applicable() {
        // 8-bit PCM has no meaningful endianness per wiki line 153
        // ("16-bit audio data, the individual PCM samples are stored
        // in big endian"); the converse is that 8-bit data is just
        // bytes.
        let mut bytes = build_minimal_film();
        patch_film_version(&mut bytes, b"1.09");
        patch_minimal_audio_fields(&mut bytes, 1, 8, 0, 22050);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        let fmt = dem.audio_format();
        match fmt {
            FilmAudioFormat::LinearPcm {
                endianness,
                sign_convention,
                ..
            } => {
                assert_eq!(endianness, PcmEndianness::NotApplicable);
                assert_eq!(sign_convention, PcmSignConvention::TwosComplement);
            }
            _ => panic!("expected LinearPcm, got {fmt:?}"),
        }
        // byte rate = 1 × 1 × 22050 = 22050.
        assert_eq!(fmt.byte_rate_bps(), Some(22050));
    }

    #[test]
    fn audio_format_sega_cd_sign_magnitude_inferred_from_null_version() {
        // NULL version + 8-bit PCM ⇒ Sega CD / 3DO era ⇒
        // sign/magnitude per wiki line 162 + 224.
        let mut bytes = build_minimal_film();
        patch_film_version(&mut bytes, b"\x00\x00\x00\x00");
        patch_minimal_audio_fields(&mut bytes, 1, 8, 0, 16000);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        match dem.audio_format() {
            FilmAudioFormat::LinearPcm {
                sign_convention, ..
            } => assert_eq!(sign_convention, PcmSignConvention::SignMagnitude),
            other => panic!("expected sign/magnitude LinearPcm, got {other:?}"),
        }
    }

    #[test]
    fn audio_format_cri_adx_adpcm_branch() {
        // audio_compression = 2 ⇒ CRI ADX ADPCM per wiki line 149.
        let mut bytes = build_minimal_film();
        patch_minimal_audio_fields(&mut bytes, 2, 0, 2, 22050);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert_eq!(
            dem.audio_format(),
            FilmAudioFormat::CriAdxAdpcm {
                channels: 2,
                sample_rate_hz: 22050,
            }
        );
        // ADX byte rate is codec-internal; not a wall-clock proxy.
        assert_eq!(dem.audio_format().byte_rate_bps(), None);
        assert!(!dem.audio_format().is_linear_pcm());
        // Audio duration also returns None for non-PCM compression.
        assert_eq!(dem.audio_duration_seconds(), None);
    }

    #[test]
    fn audio_format_unknown_compression_preserves_fields() {
        // audio_compression discriminator not in {0, 2}. Surface as
        // Unknown so caller can log / branch on it.
        let mut bytes = build_minimal_film();
        patch_minimal_audio_fields(&mut bytes, 1, 8, 7, 11025);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert_eq!(
            dem.audio_format(),
            FilmAudioFormat::Unknown {
                channels: 1,
                bits_per_sample: 8,
                sample_rate_hz: 11025,
                compression: 7,
            }
        );
        assert_eq!(dem.audio_format().byte_rate_bps(), None);
    }

    #[test]
    fn byte_rate_returns_none_on_zero_or_non_multiple_of_8_bits() {
        // A LinearPcm value with bits_per_sample = 12 (not 8/16) has
        // no well-defined byte rate.
        let fmt = FilmAudioFormat::LinearPcm {
            channels: 2,
            bits_per_sample: 12,
            sample_rate_hz: 44100,
            endianness: PcmEndianness::BigEndian,
            sign_convention: PcmSignConvention::TwosComplement,
        };
        assert_eq!(fmt.byte_rate_bps(), None);
        // Zero channels.
        let fmt = FilmAudioFormat::LinearPcm {
            channels: 0,
            bits_per_sample: 16,
            sample_rate_hz: 44100,
            endianness: PcmEndianness::BigEndian,
            sign_convention: PcmSignConvention::TwosComplement,
        };
        assert_eq!(fmt.byte_rate_bps(), None);
        // Zero sample rate.
        let fmt = FilmAudioFormat::LinearPcm {
            channels: 2,
            bits_per_sample: 16,
            sample_rate_hz: 0,
            endianness: PcmEndianness::BigEndian,
            sign_convention: PcmSignConvention::TwosComplement,
        };
        assert_eq!(fmt.byte_rate_bps(), None);
    }

    #[test]
    fn audio_duration_seconds_sums_audio_sample_lengths() {
        // FILM with 2 audio records of 4096 B and 8192 B = 12288 B.
        // At 8-bit mono 22050 Hz, byte_rate = 22050 B/s ⇒
        // duration = 12288 / 22050 ≈ 0.5574 s.
        let mut bytes =
            build_film_with_records(&[[0, 4096, 0xFFFF_FFFF, 1], [4096, 8192, 0xFFFF_FFFF, 1]]);
        // Patch FILM version + FDSC audio fields onto the canned
        // build_film_with_records output (which leaves audio fields at 0).
        patch_film_version(&mut bytes, b"1.09");
        patch_minimal_audio_fields(&mut bytes, 1, 8, 0, 22050);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        let dur = dem.audio_duration_seconds().unwrap();
        let expected = 12288.0 / 22050.0;
        assert!(
            (dur - expected).abs() < 1e-9,
            "audio duration {dur} ≠ expected {expected}"
        );
    }

    #[test]
    fn audio_duration_seconds_returns_none_when_no_audio_records() {
        // Audio declared in FDSC but no audio sample records.
        let mut bytes = build_film_with_records(&[[0, 500, 0x00000000, 1]]);
        patch_film_version(&mut bytes, b"1.07");
        patch_minimal_audio_fields(&mut bytes, 1, 8, 0, 22050);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        // No audio sample rows → no audio duration.
        assert_eq!(dem.audio_duration_seconds(), None);
    }

    #[test]
    fn well_formed_audio_requires_sample_info_2_eq_one() {
        // Wiki line 116: audio sample_info_2 must be exactly 1.
        let ok = SampleRecord {
            sample_offset: 0,
            sample_length: 1024,
            sample_info_1: 0xFFFF_FFFF,
            sample_info_2: 1,
        };
        let bad = SampleRecord {
            sample_offset: 0,
            sample_length: 1024,
            sample_info_1: 0xFFFF_FFFF,
            sample_info_2: 7,
        };
        let video = SampleRecord {
            sample_offset: 0,
            sample_length: 1024,
            sample_info_1: 0x00000000,
            sample_info_2: 20,
        };
        assert!(ok.is_well_formed_audio());
        assert!(!bad.is_well_formed_audio());
        // Video record is NOT audio at all.
        assert!(!video.is_well_formed_audio());
    }

    #[test]
    fn first_malformed_audio_sample_pinpoints_offender() {
        // 3 audio records, middle one has sample_info_2 = 2.
        let bytes = build_film_with_records(&[
            [0, 1024, 0xFFFF_FFFF, 1],
            [1024, 1024, 0xFFFF_FFFF, 2],
            [2048, 1024, 0xFFFF_FFFF, 1],
        ]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert_eq!(dem.first_malformed_audio_sample(), Some(1));

        // All well-formed → None.
        let bytes =
            build_film_with_records(&[[0, 1024, 0xFFFF_FFFF, 1], [1024, 1024, 0xFFFF_FFFF, 1]]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert_eq!(dem.first_malformed_audio_sample(), None);

        // No audio at all → None (vacuously well-formed).
        let bytes = build_film_with_records(&[[0, 500, 0x00000000, 20]]);
        let dem = FilmDemuxer::parse(&bytes).unwrap();
        assert_eq!(dem.first_malformed_audio_sample(), None);
    }

    #[test]
    fn audio_format_unaffected_by_strip_count_or_video_fields() {
        // The FDSC's `bits_per_pixel`, `height`, `width`,
        // `video_codec` are irrelevant to the audio classifier — vary
        // them and observe identical results.
        let mut a = build_minimal_film();
        patch_film_version(&mut a, b"1.07");
        patch_minimal_audio_fields(&mut a, 2, 16, 0, 44100);
        let mut b = a.clone();
        // Patch the video FOURCC and bpp.
        b[16 + 8..16 + 12].copy_from_slice(b"cvid"); // already cvid; no-op
        b[16 + 20] = 16; // bpp 16 vs 24
        let dem_a = FilmDemuxer::parse(&a).unwrap();
        let dem_b = FilmDemuxer::parse(&b).unwrap();
        assert_eq!(dem_a.audio_format(), dem_b.audio_format());
    }

    #[test]
    fn abbreviated_fdsc_reports_no_audio() {
        // The 0x14-byte abbreviated FDSC has no audio fields; parser
        // zero-fills them and `has_audio()` reports false.
        let mut out = Vec::new();
        out.extend_from_slice(b"FILM");
        out.extend_from_slice(&((16 + 0x14 + 16) as u32).to_be_bytes());
        out.extend_from_slice(b"\x00\x02\x00\x00");
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(b"FDSC");
        out.extend_from_slice(&0x14u32.to_be_bytes());
        out.extend_from_slice(b"cvid");
        out.extend_from_slice(&80u32.to_be_bytes());
        out.extend_from_slice(&80u32.to_be_bytes());
        out.extend_from_slice(b"STAB");
        out.extend_from_slice(&16u32.to_be_bytes());
        out.extend_from_slice(&30u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        let dem = FilmDemuxer::parse(&out).unwrap();
        assert!(!dem.fdsc.has_audio());
        assert_eq!(dem.audio_format(), FilmAudioFormat::None);
    }

    // ---- r261: Samples typed iterator over STAB records ---------------

    /// Helper to encode a 16-byte sample record into raw big-endian
    /// bytes, the same wire format the STAB record table uses.
    fn enc_record(off: u32, len: u32, si1: u32, si2: u32) -> [u8; 16] {
        let mut r = [0u8; 16];
        r[0..4].copy_from_slice(&off.to_be_bytes());
        r[4..8].copy_from_slice(&len.to_be_bytes());
        r[8..12].copy_from_slice(&si1.to_be_bytes());
        r[12..16].copy_from_slice(&si2.to_be_bytes());
        r
    }

    #[test]
    fn samples_empty_records_yields_none() {
        let it = Samples::new(&[]).unwrap();
        assert_eq!(it.remaining(), 0);
        assert_eq!(it.cursor(), 0);
        assert_eq!(it.next_index(), 0);
        let collected: Vec<_> = it.collect();
        assert!(collected.is_empty());
    }

    #[test]
    fn samples_rejects_misaligned_slice() {
        // 17 bytes is not a multiple of 16.
        let buf = [0u8; 17];
        assert!(Samples::new(&buf).is_err());
        // 0 bytes is the legal empty case.
        assert!(Samples::new(&buf[..0]).is_ok());
        // 16 bytes is one record.
        assert!(Samples::new(&buf[..16]).is_ok());
        // 15 bytes (just under one) is rejected.
        assert!(Samples::new(&buf[..15]).is_err());
    }

    #[test]
    fn samples_single_video_keyframe() {
        // Sample 0 from build_minimal_film(): offset=0, len=500,
        // si1=0 (key, ts=0), si2=1.
        let rec = enc_record(0, 500, 0x0000_0000, 1);
        let mut it = Samples::new(&rec).unwrap();
        assert_eq!(it.remaining(), 1);
        let e = it.next().unwrap();
        assert_eq!(e.index, 0);
        assert_eq!(e.record.sample_offset, 0);
        assert_eq!(e.record.sample_length, 500);
        assert_eq!(
            e.kind,
            SampleKind::VideoKeyframe {
                timestamp_ticks: 0,
                next_frame_ticks: 1,
            }
        );
        assert!(e.kind.is_keyframe());
        assert!(e.kind.is_video());
        assert!(!e.kind.is_audio());
        assert_eq!(it.next_index(), 1);
        assert_eq!(it.remaining(), 0);
        assert!(it.next().is_none());
    }

    /// Mirrors the spec §2.7 fixture `C3` STAB layout: 5 video
    /// samples, sample 0 keyframe at ts=0, samples 1-4 inter with
    /// ascending timestamps.
    #[test]
    fn samples_c3_fixture_five_video_samples() {
        let mut records = Vec::with_capacity(5 * 16);
        records.extend_from_slice(&enc_record(0, 5177, 0x0000_0000, 1));
        records.extend_from_slice(&enc_record(0x1439, 2590, 0x8000_0001, 1));
        records.extend_from_slice(&enc_record(0x1e57, 2400, 0x8000_0002, 1));
        records.extend_from_slice(&enc_record(0x27b7, 2300, 0x8000_0003, 1));
        records.extend_from_slice(&enc_record(0x30b3, 2200, 0x8000_0004, 1));
        let it = Samples::new(&records).unwrap();
        assert_eq!(it.remaining(), 5);
        let entries: Vec<_> = it.collect();
        assert_eq!(entries.len(), 5);
        assert_eq!(
            entries[0].kind,
            SampleKind::VideoKeyframe {
                timestamp_ticks: 0,
                next_frame_ticks: 1,
            }
        );
        for (i, e) in entries.iter().enumerate().skip(1) {
            assert_eq!(e.index as usize, i);
            assert_eq!(
                e.kind,
                SampleKind::VideoInter {
                    timestamp_ticks: i as u32,
                    next_frame_ticks: 1,
                }
            );
        }
    }

    #[test]
    fn samples_audio_sentinel_classified() {
        // sample_info_1 == 0xFFFFFFFF flags audio; sample_info_2 == 1
        // is the well-formed audio constant.
        let rec = enc_record(0x500, 100, 0xFFFF_FFFF, 1);
        let mut it = Samples::new(&rec).unwrap();
        let e = it.next().unwrap();
        assert_eq!(e.kind, SampleKind::Audio { well_formed: true });
        assert!(e.kind.is_audio());
        assert!(!e.kind.is_video());
        assert!(!e.kind.is_keyframe());
        assert_eq!(e.record.sample_info_1, 0xFFFF_FFFF);
        assert_eq!(e.record.sample_info_2, 1);
    }

    #[test]
    fn samples_audio_malformed_si2_flagged() {
        // si1 == 0xFFFFFFFF is still audio, but si2 != 1 means the
        // wiki's audio-interleave invariant is broken.
        let rec = enc_record(0x500, 100, 0xFFFF_FFFF, 7);
        let mut it = Samples::new(&rec).unwrap();
        let e = it.next().unwrap();
        assert_eq!(e.kind, SampleKind::Audio { well_formed: false });
    }

    /// Interleaved video + audio + video classification end-to-end.
    #[test]
    fn samples_interleaved_video_audio() {
        let mut records = Vec::new();
        records.extend_from_slice(&enc_record(0, 1000, 0x0000_0000, 1)); // V key
        records.extend_from_slice(&enc_record(0x3e8, 200, 0xFFFF_FFFF, 1)); // A
        records.extend_from_slice(&enc_record(0x4b0, 800, 0x8000_0014, 20)); // V inter
        let entries: Vec<_> = Samples::new(&records).unwrap().collect();
        assert!(matches!(entries[0].kind, SampleKind::VideoKeyframe { .. }));
        assert!(matches!(entries[1].kind, SampleKind::Audio { .. }));
        assert!(matches!(entries[2].kind, SampleKind::VideoInter { .. }));
        if let SampleKind::VideoInter {
            timestamp_ticks,
            next_frame_ticks,
        } = entries[2].kind
        {
            assert_eq!(timestamp_ticks, 0x14);
            assert_eq!(next_frame_ticks, 20);
        } else {
            unreachable!();
        }
    }

    /// Top bit set + non-zero low 31 bits ⇒ inter; timestamp is the
    /// low 31 bits.
    #[test]
    fn samples_inter_timestamp_strips_top_bit() {
        let rec = enc_record(0, 1234, 0x8000_007B, 5);
        let mut it = Samples::new(&rec).unwrap();
        let e = it.next().unwrap();
        assert_eq!(
            e.kind,
            SampleKind::VideoInter {
                timestamp_ticks: 0x7B,
                next_frame_ticks: 5,
            }
        );
    }

    /// Top bit set with low 31 bits == max (0x7FFFFFFF) — saturates
    /// at the 31-bit timestamp ceiling.
    #[test]
    fn samples_inter_timestamp_max_low31() {
        let rec = enc_record(0, 0, 0xFFFF_FFFE, 1);
        let mut it = Samples::new(&rec).unwrap();
        let e = it.next().unwrap();
        assert_eq!(
            e.kind,
            SampleKind::VideoInter {
                timestamp_ticks: 0x7FFF_FFFE,
                next_frame_ticks: 1,
            }
        );
    }

    /// Cursor + next_index advance one record at a time, matching the
    /// per-iteration step of `SAMPLE_RECORD_SIZE` bytes.
    #[test]
    fn samples_cursor_and_next_index_advance() {
        let mut records = Vec::new();
        records.extend_from_slice(&enc_record(0, 100, 0, 1));
        records.extend_from_slice(&enc_record(100, 200, 0x8000_0001, 1));
        records.extend_from_slice(&enc_record(300, 0, 0xFFFF_FFFF, 1));
        let mut it = Samples::new(&records).unwrap();
        assert_eq!(it.cursor(), 0);
        assert_eq!(it.next_index(), 0);
        it.next().unwrap();
        assert_eq!(it.cursor(), SAMPLE_RECORD_SIZE);
        assert_eq!(it.next_index(), 1);
        it.next().unwrap();
        assert_eq!(it.cursor(), 2 * SAMPLE_RECORD_SIZE);
        assert_eq!(it.next_index(), 2);
        it.next().unwrap();
        assert_eq!(it.cursor(), 3 * SAMPLE_RECORD_SIZE);
        assert_eq!(it.next_index(), 3);
        assert!(it.next().is_none());
        assert_eq!(it.remaining(), 0);
    }

    /// `ExactSizeIterator` reports the correct count both before and
    /// after pulling items.
    #[test]
    fn samples_exact_size_hint() {
        let records: Vec<u8> = (0..4u32)
            .flat_map(|i| enc_record(i * 100, 100, 0x8000_0000 | i, 1).to_vec())
            .collect();
        let mut it = Samples::new(&records).unwrap();
        assert_eq!(it.size_hint(), (4, Some(4)));
        assert_eq!(it.len(), 4);
        it.next();
        assert_eq!(it.size_hint(), (3, Some(3)));
        assert_eq!(it.len(), 3);
        for _ in &mut it {}
        assert_eq!(it.size_hint(), (0, Some(0)));
    }

    /// `Samples` is a `FusedIterator` — once exhausted, every
    /// subsequent `next()` returns `None`.
    #[test]
    fn samples_fused_after_exhaustion() {
        let rec = enc_record(0, 0, 0, 1);
        let mut it = Samples::new(&rec).unwrap();
        assert!(it.next().is_some());
        assert!(it.next().is_none());
        assert!(it.next().is_none());
        assert!(it.next().is_none());
    }

    /// `records_bytes()` returns the original slice untouched.
    #[test]
    fn samples_records_bytes_round_trip() {
        let records: Vec<u8> = (0..3u32)
            .flat_map(|i| enc_record(i, i + 1, i + 2, i + 3).to_vec())
            .collect();
        let it = Samples::new(&records).unwrap();
        assert_eq!(it.records_bytes(), &records[..]);
    }

    /// Per-yield classification matches the existing
    /// `SampleRecord::is_audio` / `is_keyframe` predicates on the
    /// underlying record — the iterator's typed enum is a strict
    /// re-statement of those, not a re-interpretation.
    #[test]
    fn samples_kind_matches_record_predicates() {
        let mut records = Vec::new();
        records.extend_from_slice(&enc_record(0, 0, 0x0000_0000, 1)); // V key
        records.extend_from_slice(&enc_record(0, 0, 0x8000_0005, 1)); // V inter
        records.extend_from_slice(&enc_record(0, 0, 0xFFFF_FFFF, 1)); // Audio
        for e in Samples::new(&records).unwrap() {
            assert_eq!(e.kind.is_audio(), e.record.is_audio());
            assert_eq!(e.kind.is_keyframe(), e.record.is_keyframe());
        }
    }

    /// `Samples` driven over a real `FilmDemuxer`-built buffer
    /// agrees with `FilmDemuxer::samples` field-by-field. This
    /// validates that the typed iterator decodes the same wire bytes
    /// the demuxer does.
    #[test]
    fn samples_agrees_with_film_demuxer() {
        // Reuse the parser path: build a multi-record FILM, parse it,
        // then re-walk the records via Samples::new from the raw
        // STAB-records suffix of the buffer.
        let mut out = build_minimal_film();
        // Patch num_entries from 1 to 3 + append two records (audio + inter).
        let num_off = 16 + 32 + 12;
        out[num_off..num_off + 4].copy_from_slice(&3u32.to_be_bytes());
        out.extend_from_slice(&enc_record(0x500, 100, 0xFFFF_FFFF, 1));
        out.extend_from_slice(&enc_record(0x6c0, 400, 0x8000_0002, 1));
        // Bump header_length to include the two extra 16-byte records.
        out[4..8].copy_from_slice(&((80 + 32) as u32).to_be_bytes());
        let dem = FilmDemuxer::parse(&out).unwrap();
        assert_eq!(dem.samples.len(), 3);

        // STAB records start after FILM header (16) + FDSC (32) +
        // STAB header (16) = 64.
        let records_start = 16 + 32 + 16;
        let records_end = records_start + 3 * SAMPLE_RECORD_SIZE;
        let it_records = &out[records_start..records_end];
        let typed: Vec<_> = Samples::new(it_records).unwrap().collect();
        assert_eq!(typed.len(), dem.samples.len());
        for (e, expected) in typed.iter().zip(dem.samples.iter()) {
            assert_eq!(e.record, *expected);
        }
        // First record is a keyframe (build_minimal_film default).
        assert!(matches!(typed[0].kind, SampleKind::VideoKeyframe { .. }));
        // Second record is the appended audio sentinel.
        assert!(matches!(typed[1].kind, SampleKind::Audio { .. }));
        // Third record is an inter frame at ts=2.
        assert!(matches!(
            typed[2].kind,
            SampleKind::VideoInter {
                timestamp_ticks: 2,
                next_frame_ticks: 1,
            }
        ));
    }

    /// `SampleKind::from_record` is the single classification surface
    /// — the iterator just re-uses it. Verify the standalone helper
    /// matches the same set of wire patterns.
    #[test]
    fn sample_kind_from_record_standalone() {
        let key = SampleRecord {
            sample_offset: 0,
            sample_length: 0,
            sample_info_1: 0x0000_0042,
            sample_info_2: 7,
        };
        assert_eq!(
            SampleKind::from_record(key),
            SampleKind::VideoKeyframe {
                timestamp_ticks: 0x42,
                next_frame_ticks: 7,
            }
        );
        let inter = SampleRecord {
            sample_offset: 0,
            sample_length: 0,
            sample_info_1: 0x8000_0042,
            sample_info_2: 7,
        };
        assert_eq!(
            SampleKind::from_record(inter),
            SampleKind::VideoInter {
                timestamp_ticks: 0x42,
                next_frame_ticks: 7,
            }
        );
        let audio_ok = SampleRecord {
            sample_offset: 0,
            sample_length: 0,
            sample_info_1: 0xFFFF_FFFF,
            sample_info_2: 1,
        };
        assert_eq!(
            SampleKind::from_record(audio_ok),
            SampleKind::Audio { well_formed: true }
        );
        let audio_bad = SampleRecord {
            sample_offset: 0,
            sample_length: 0,
            sample_info_1: 0xFFFF_FFFF,
            sample_info_2: 99,
        };
        assert_eq!(
            SampleKind::from_record(audio_bad),
            SampleKind::Audio { well_formed: false }
        );
    }

    /// Top-bit clear with non-zero low 31 bits is still a keyframe
    /// (only the top bit distinguishes intra-vs-inter, not the
    /// timestamp value).
    #[test]
    fn samples_keyframe_with_nonzero_ts() {
        let rec = enc_record(0, 0, 0x0000_03e8, 1);
        let mut it = Samples::new(&rec).unwrap();
        let e = it.next().unwrap();
        assert_eq!(
            e.kind,
            SampleKind::VideoKeyframe {
                timestamp_ticks: 1000,
                next_frame_ticks: 1,
            }
        );
    }

    // ---- StabHeader::parse_chunk -- STAB chunk header → records slice ---

    /// Helper to build a STAB chunk: 16-byte header (signature,
    /// length, base_frequency, num_entries) followed by the raw
    /// concatenated record bytes. `chunk_length` lets a test choose
    /// whether the on-wire length field accounts for the first 16
    /// bytes (it is validation-only and must not affect parsing).
    fn enc_stab_chunk(base_freq: u32, records: &[u8], chunk_length: u32) -> Vec<u8> {
        assert_eq!(records.len() % 16, 0);
        let num_entries = (records.len() / 16) as u32;
        let mut out = Vec::with_capacity(16 + records.len());
        out.extend_from_slice(b"STAB");
        out.extend_from_slice(&chunk_length.to_be_bytes());
        out.extend_from_slice(&base_freq.to_be_bytes());
        out.extend_from_slice(&num_entries.to_be_bytes());
        out.extend_from_slice(records);
        out
    }

    #[test]
    fn stab_header_const_matches_record_header_split() {
        // The STAB fixed header is the same 16 bytes the FILM demuxer
        // skips before the record table.
        assert_eq!(STAB_HEADER_SIZE, 16);
    }

    #[test]
    fn stab_parse_chunk_empty_table() {
        let chunk = enc_stab_chunk(30, &[], 16);
        let (hdr, recs) = StabHeader::parse_chunk(&chunk).unwrap();
        assert_eq!(hdr.base_frequency, 30);
        assert_eq!(hdr.num_entries, 0);
        assert!(recs.is_empty());
        // Records slice feeds Samples::new with no rows.
        let it = Samples::new(recs).unwrap();
        assert_eq!(it.remaining(), 0);
    }

    #[test]
    fn stab_parse_chunk_single_record_feeds_samples() {
        let rec = enc_record(0, 500, 0x0000_0000, 1);
        let chunk = enc_stab_chunk(600, &rec, 16 + 16);
        let (hdr, recs) = StabHeader::parse_chunk(&chunk).unwrap();
        assert_eq!(hdr.base_frequency, 600);
        assert_eq!(hdr.num_entries, 1);
        assert_eq!(recs.len(), 16);
        assert_eq!(recs, &rec[..]);
        let mut it = Samples::new(recs).unwrap();
        let e = it.next().unwrap();
        assert_eq!(e.index, 0);
        assert_eq!(e.record.sample_length, 500);
        assert_eq!(
            e.kind,
            SampleKind::VideoKeyframe {
                timestamp_ticks: 0,
                next_frame_ticks: 1,
            }
        );
        assert!(it.next().is_none());
    }

    /// The C3-style 5-sample table: parse_chunk + Samples must yield
    /// the same classified stream the round-261 records-only test
    /// produced.
    #[test]
    fn stab_parse_chunk_c3_five_samples() {
        let mut records = Vec::with_capacity(5 * 16);
        records.extend_from_slice(&enc_record(0, 5177, 0x0000_0000, 1));
        records.extend_from_slice(&enc_record(0x1439, 2590, 0x8000_0001, 1));
        records.extend_from_slice(&enc_record(0x1e57, 2400, 0x8000_0002, 1));
        records.extend_from_slice(&enc_record(0x27b7, 2300, 0x8000_0003, 1));
        records.extend_from_slice(&enc_record(0x30b3, 2200, 0x8000_0004, 1));
        let chunk = enc_stab_chunk(600, &records, 16 + 5 * 16);
        let (hdr, recs) = StabHeader::parse_chunk(&chunk).unwrap();
        assert_eq!(hdr.num_entries, 5);
        assert_eq!(recs.len(), 5 * 16);
        let entries: Vec<_> = Samples::new(recs).unwrap().collect();
        assert_eq!(entries.len(), 5);
        assert!(entries[0].kind.is_keyframe());
        for (i, e) in entries.iter().enumerate().skip(1) {
            assert_eq!(
                e.kind,
                SampleKind::VideoInter {
                    timestamp_ticks: i as u32,
                    next_frame_ticks: 1,
                }
            );
        }
    }

    /// `Sega_FILM.wiki` line 92: some titles omit the first 16 bytes
    /// from the STAB length field. parse_chunk must ignore the length
    /// field for offset arithmetic and rely on `num_entries` — so a
    /// "short" length still parses the records correctly.
    #[test]
    fn stab_parse_chunk_ignores_short_length_field() {
        let rec = enc_record(7, 11, 0x8000_0005, 3);
        // Burning-Rangers-style: length omits the first 16 bytes (only
        // the record table is counted: 16, not 32).
        let chunk = enc_stab_chunk(30, &rec, 16);
        let (hdr, recs) = StabHeader::parse_chunk(&chunk).unwrap();
        assert_eq!(hdr.base_frequency, 30);
        assert_eq!(hdr.num_entries, 1);
        let e = Samples::new(recs).unwrap().next().unwrap();
        assert_eq!(e.record.sample_offset, 7);
        assert_eq!(e.record.sample_length, 11);
        assert_eq!(
            e.kind,
            SampleKind::VideoInter {
                timestamp_ticks: 5,
                next_frame_ticks: 3,
            }
        );
    }

    #[test]
    fn stab_parse_chunk_trailing_bytes_excluded() {
        let rec = enc_record(0, 1, 0, 1);
        let mut chunk = enc_stab_chunk(30, &rec, 32);
        // Append stray trailing bytes beyond the declared 1-record
        // table; they must not be returned in the records slice.
        chunk.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        let (hdr, recs) = StabHeader::parse_chunk(&chunk).unwrap();
        assert_eq!(hdr.num_entries, 1);
        assert_eq!(recs.len(), 16);
        assert_eq!(recs, &rec[..]);
    }

    #[test]
    fn stab_parse_chunk_rejects_truncated_header() {
        let buf = [0u8; 15];
        assert!(StabHeader::parse_chunk(&buf).is_err());
        // Exactly 16 bytes with 0 entries is the legal minimum.
        let chunk = enc_stab_chunk(30, &[], 16);
        assert!(StabHeader::parse_chunk(&chunk).is_ok());
    }

    #[test]
    fn stab_parse_chunk_rejects_bad_signature() {
        let mut chunk = enc_stab_chunk(30, &[], 16);
        chunk[0] = b'X';
        assert!(StabHeader::parse_chunk(&chunk).is_err());
    }

    #[test]
    fn stab_parse_chunk_rejects_truncated_records() {
        // Header declares 2 entries but only 1 record's worth of bytes
        // follow the 16-byte header.
        let rec = enc_record(0, 1, 0, 1);
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"STAB");
        chunk.extend_from_slice(&48u32.to_be_bytes()); // length claims 2 records
        chunk.extend_from_slice(&30u32.to_be_bytes());
        chunk.extend_from_slice(&2u32.to_be_bytes()); // num_entries = 2
        chunk.extend_from_slice(&rec); // but only 1 record present
        assert!(StabHeader::parse_chunk(&chunk).is_err());
    }

    /// parse_chunk applied to the STAB chunk carved out of a full FILM
    /// header (FILM + FDSC + STAB) must agree with FilmDemuxer::parse
    /// on both the header fields and the per-record stream.
    #[test]
    fn stab_parse_chunk_agrees_with_film_demuxer() {
        let film = build_minimal_film();
        let dem = FilmDemuxer::parse(&film).unwrap();
        // The STAB chunk begins right after FILM (16) + FDSC (32).
        let stab_off = FILM_HEADER_MIN_SIZE + 32;
        let (hdr, recs) = StabHeader::parse_chunk(&film[stab_off..]).unwrap();
        assert_eq!(hdr, dem.stab_header);
        let entries: Vec<_> = Samples::new(recs).unwrap().collect();
        assert_eq!(entries.len(), dem.samples.len());
        for (e, d) in entries.iter().zip(dem.samples.iter()) {
            assert_eq!(e.record, *d);
        }
    }
}
