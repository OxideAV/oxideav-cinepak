//! `oxideav-core` integration layer for `oxideav-cinepak`.
//!
//! Gated behind the default-on `registry` feature. Wires up:
//! - `From<CinepakError> for oxideav_core::Error`,
//! - `From<CinepakFrame> for oxideav_core::VideoFrame`,
//! - `From<CinepakPixelFormat> for oxideav_core::PixelFormat`,
//! - the `Decoder` trait implementation,
//! - the `register_codecs` / `register` entry points the umbrella
//!   `oxideav` crate calls during framework initialisation.

use oxideav_core::frame::VideoPlane;
use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, CodecTag, Decoder,
    Error, Frame, Packet, PixelFormat, ProbeContext, Result, RuntimeContext, VideoFrame,
};

use crate::decoder::CinepakDecoder;
use crate::error::CinepakError;
use crate::header::{FRAME_HEADER_SIZE, STRIP_HEADER_SIZE, STRIP_ID_INTER, STRIP_ID_INTRA};
use crate::image::{CinepakFrame, CinepakPixelFormat, CinepakPlane};
use crate::CODEC_ID_STR;

// ---- Error / pixel-format / frame conversions --------------------------

impl From<CinepakError> for Error {
    fn from(e: CinepakError) -> Self {
        match e {
            CinepakError::InvalidData(s) => Error::InvalidData(s),
            CinepakError::Unsupported(s) => Error::Unsupported(s),
            CinepakError::Other(s) => Error::Other(s),
        }
    }
}

impl From<CinepakPixelFormat> for PixelFormat {
    fn from(p: CinepakPixelFormat) -> Self {
        match p {
            CinepakPixelFormat::Rgb24 => PixelFormat::Rgb24,
            CinepakPixelFormat::Gray8 => PixelFormat::Gray8,
        }
    }
}

impl From<CinepakPlane> for VideoPlane {
    fn from(p: CinepakPlane) -> Self {
        VideoPlane {
            stride: p.stride,
            data: p.data,
        }
    }
}

impl From<CinepakFrame> for VideoFrame {
    fn from(f: CinepakFrame) -> Self {
        VideoFrame {
            pts: f.pts,
            planes: f.planes.into_iter().map(VideoPlane::from).collect(),
        }
    }
}

// ---- Registry entry points ---------------------------------------------

/// Register the Cinepak (`CVID`) decoder into the supplied
/// [`CodecRegistry`]. Cinepak FourCC is `CVID` (AVI `biCompression`,
/// QuickTime `cvid` codec tag, Sega FILM `cvid`); decoder tag claims
/// upper-case `CVID` per the workspace convention.
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("cinepak_sw")
        .with_lossy(true)
        // Cinepak is intra-/inter-coded but skip macroblocks reuse
        // previous-frame reconstruction, not motion vectors. The codec
        // has key (intra) and delta (inter) frames; not intra-only.
        .with_intra_only(false)
        // Cinepak's frame_length is a 24-bit field (max ~16 MiB) but the
        // codec was designed for tiny CD-era resolutions; cap at 4 K to
        // catch obvious malformed streams.
        .with_max_size(4096, 4096);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .probe(probe_cvid)
            .tags([
                CodecTag::fourcc(b"CVID"),
                // QuickTime sample-entry tag `cvid` is also legal — the
                // CodecTag::fourcc constructor upper-cases.
                CodecTag::fourcc(b"cvid"),
            ]),
    );
}

/// Disambiguating probe for `CVID` FourCC. Confirms the first packet's
/// 10-byte Cinepak frame header is structurally valid:
/// - `flags & 0xfe == 0` (only bit 0 is defined),
/// - `frame_length >= 10 + 12` (header + at least one strip header),
/// - `width` and `height` non-zero, multiples of 4, ≤ 4096,
/// - `strip_count >= 1`,
/// - the first strip's `strip_id` is `0x1000` (intra) or `0x1100` (inter),
/// - the first strip's `strip_size >= 12` and fits inside `frame_length`.
///
/// When no packet is available, returns `0.5` — Cinepak's `'cvid'`
/// FourCC is rare enough to outweigh similar-named codecs even on weak
/// evidence (no other codec in the registry currently claims `'cvid'`).
pub fn probe_cvid(ctx: &ProbeContext) -> f32 {
    let Some(pkt) = ctx.packet else {
        return 0.5;
    };
    if pkt.len() < FRAME_HEADER_SIZE + STRIP_HEADER_SIZE {
        return 0.0;
    }
    // Frame header structural checks.
    let flags = pkt[0];
    if flags & 0xfe != 0 {
        return 0.0;
    }
    let frame_length = (u32::from(pkt[1]) << 16) | (u32::from(pkt[2]) << 8) | u32::from(pkt[3]);
    if (frame_length as usize) < FRAME_HEADER_SIZE + STRIP_HEADER_SIZE {
        return 0.0;
    }
    let width = u16::from_be_bytes([pkt[4], pkt[5]]);
    let height = u16::from_be_bytes([pkt[6], pkt[7]]);
    if width == 0 || height == 0 || width % 4 != 0 || height % 4 != 0 {
        return 0.0;
    }
    if width > 4096 || height > 4096 {
        return 0.0;
    }
    let strip_count = u16::from_be_bytes([pkt[8], pkt[9]]);
    if strip_count == 0 {
        return 0.0;
    }
    // First strip header.
    let strip_id = u16::from_be_bytes([pkt[10], pkt[11]]);
    if strip_id != STRIP_ID_INTRA && strip_id != STRIP_ID_INTER {
        return 0.0;
    }
    let strip_size = u16::from_be_bytes([pkt[12], pkt[13]]);
    if (strip_size as usize) < STRIP_HEADER_SIZE {
        return 0.0;
    }
    if (FRAME_HEADER_SIZE + strip_size as usize) > frame_length as usize {
        return 0.0;
    }
    1.0
}

/// Unified entry point: install the Cinepak codec into a
/// [`RuntimeContext`].
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
}

oxideav_core::register!("cinepak", register);

// ---- Decoder trait impl ------------------------------------------------

pub fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    let codec_id = params.codec_id.clone();
    Ok(Box::new(CinepakDecoderHandle {
        codec_id,
        inner: CinepakDecoder::new(),
        pending: None,
        eof: false,
    }))
}

struct CinepakDecoderHandle {
    codec_id: CodecId,
    inner: CinepakDecoder,
    pending: Option<Packet>,
    eof: bool,
}

impl Decoder for CinepakDecoderHandle {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if self.pending.is_some() {
            return Err(Error::other(
                "Cinepak decoder: receive_frame must be called before sending another packet",
            ));
        }
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        let Some(pkt) = self.pending.take() else {
            return if self.eof {
                Err(Error::Eof)
            } else {
                Err(Error::NeedMore)
            };
        };
        let frame = self.inner.decode_frame(&pkt.data, pkt.pts)?;
        Ok(Frame::Video(frame.into()))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.inner.reset();
        self.pending = None;
        self.eof = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_cvid_fourcc() {
        let mut ctx = RuntimeContext::new();
        super::register(&mut ctx);
        assert!(ctx.codecs.decoder_ids().any(|id| id.as_str() == "cinepak"));
    }

    fn build_minimal_cvid_packet() -> Vec<u8> {
        // 10-byte frame header + 12-byte strip header, no chunks. Not a
        // *complete* decodable frame, but structurally valid for the probe.
        let mut bytes = Vec::new();
        // flags=0, frame_length=22, width=8, height=8, strip_count=1
        bytes.extend_from_slice(&[0, 0, 0, 22, 0, 8, 0, 8, 0, 1]);
        // strip_id=0x1000, strip_size=12, y_top=0, x_top=0, y_bottom=8, x_bottom=8
        bytes.extend_from_slice(&[0x10, 0x00, 0, 12, 0, 0, 0, 0, 0, 8, 0, 8]);
        bytes
    }

    #[test]
    fn probe_accepts_valid_cvid_header() {
        let pkt = build_minimal_cvid_packet();
        let tag = CodecTag::fourcc(b"CVID");
        let ctx = ProbeContext::new(&tag).packet(&pkt);
        assert_eq!(super::probe_cvid(&ctx), 1.0);
    }

    #[test]
    fn probe_rejects_misaligned_dims() {
        let mut pkt = build_minimal_cvid_packet();
        // Patch width to 7 (not multiple of 4).
        pkt[4..6].copy_from_slice(&7u16.to_be_bytes());
        let tag = CodecTag::fourcc(b"CVID");
        let ctx = ProbeContext::new(&tag).packet(&pkt);
        assert_eq!(super::probe_cvid(&ctx), 0.0);
    }

    #[test]
    fn probe_rejects_bad_strip_id() {
        let mut pkt = build_minimal_cvid_packet();
        // Patch strip_id to 0x1234.
        pkt[10..12].copy_from_slice(&0x1234u16.to_be_bytes());
        let tag = CodecTag::fourcc(b"CVID");
        let ctx = ProbeContext::new(&tag).packet(&pkt);
        assert_eq!(super::probe_cvid(&ctx), 0.0);
    }

    #[test]
    fn probe_returns_partial_confidence_without_packet() {
        let tag = CodecTag::fourcc(b"CVID");
        let ctx = ProbeContext::new(&tag);
        assert_eq!(super::probe_cvid(&ctx), 0.5);
    }
}
