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
    Error, Frame, Packet, PixelFormat, Result, RuntimeContext, VideoFrame,
};

use crate::decoder::CinepakDecoder;
use crate::error::CinepakError;
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
            .tags([
                CodecTag::fourcc(b"CVID"),
                // QuickTime sample-entry tag `cvid` is also legal — the
                // CodecTag::fourcc constructor upper-cases.
                CodecTag::fourcc(b"cvid"),
            ]),
    );
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
}
