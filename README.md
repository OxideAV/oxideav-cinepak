# oxideav-cinepak

Pure-Rust **Cinepak (CVID)** video decoder for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.
Cinepak (Radius/Apple, 1991) is the vector-quantisation codec that
shipped with mid-1990s QuickTime and AVI clips. Zero C dependencies.

## Status

| Feature                                            | Status |
|----------------------------------------------------|--------|
| Frame + strip + chunk parsing                      | works  |
| V1 + V4 codebook full updates (6-byte entries)     | works  |
| V1 + V4 codebook full updates (4-byte / paletted)  | works  |
| Selective codebook updates (256-bit bitmap)        | works  |
| INTRA vector lists (`0x30`)                        | works  |
| INTER vector lists (`0x31`) with MB skip           | works  |
| V1-only vector lists (`0x32`)                      | works  |
| Cross-strip codebook inheritance                   | works  |
| `strip_y1 == 0` running-y-cursor overload          | works  |
| Output `PixelFormat::Yuv420P`                      | works  |

## Gaps

- **8-bit paletted output** (`bits_per_coded_sample == 8`) is not
  implemented. We always render YUV420P with the unsigned-chroma
  mapping `Cb = u + 128`, `Cr = v + 128`.
- **Sega FILM 2/6-byte filler probing** between the frame header and
  the first strip header is not implemented (no fixture in the trace
  corpus required it).
- **Encoder** — not implemented. Cinepak encoders need an LBG / ELBG
  vector-quantiser training pass and we have no use case for that.
- The **chroma-to-RGB matrix** described in trace-doc §5.1 is not
  applied; we keep the codec's native YUV-like representation. Re-
  conversion to RGB for display is a downstream concern.

## Installation

```toml
[dependencies]
oxideav-core = "0.1"
oxideav-cinepak = "0.0"
```

## Quick use

```rust,no_run
use oxideav_core::CodecRegistry;

let mut codecs = CodecRegistry::new();
oxideav_cinepak::register(&mut codecs);
```

The decoder claims AVI FOURCCs `cvid` and `CVID`; downstream container
crates (oxideav-mp4, oxideav-avi) recognise both.

## References

- `docs/video/cinepak/cinepak-trace-reverse-engineering.md` —
  primary clean-room spec extracted from instrumented FFmpeg traces
  (no FFmpeg source quoted).
- Tim Ferguson, *The Cinepak Video Codec — A Reverse Engineering
  Document*, http://multimedia.cx — community write-up.
- multimedia.cx wiki, *Cinepak (CVID)*.

## License

MIT.
