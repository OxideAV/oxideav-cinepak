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
}

/// Sample Table (`STAB`) header (excluding the per-sample records).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StabHeader {
    /// Sample-rate base for `sample_info_1` timestamps, in Hz.
    pub base_frequency: u32,
    /// Number of 16-byte sample records that follow.
    pub num_entries: u32,
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
}

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
}
