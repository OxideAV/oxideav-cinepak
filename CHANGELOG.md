# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Round 1 clean-room rebuild from `docs/video/cinepak/spec/`. Decode
  path covers:
  - 10-byte frame header parse (`flags`, 24-bit `frame_length`,
    `width`, `height`, `strip_count`).
  - 12-byte strip header parse with the Y-coordinate sentinel rule
    (`y_top == 0` on non-first strips means "starts at previous
    `y_bottom`; the wire `y_bottom` is the strip's height").
  - Codebook chunks `0x20xx` / `0x21xx` / `0x22xx` / `0x23xx` (12-bit
    YUV V4/V1 full + selective) and `0x24xx` / `0x25xx` / `0x26xx` /
    `0x27xx` (8-bit grayscale equivalents).
  - Vector chunks `0x3200` (V1-only intra), `0x3000` (mixed V1/V4
    intra), `0x3100` (inter with `0` skip / `10` V1 / `11` V4 codes;
    selector-bit spillover across flag-word boundaries handled).
  - V1 quadrant expansion and V4 sub-block expansion of 4×4
    macroblocks, plus YUV → RGB matrix with truncation-toward-zero
    on `U / 2` and `[0, 255]` per-channel clamp.
  - Codebook inheritance across strips and across frames.
  - Skip macroblocks copy from the previous frame's reconstructed
    pixel buffer.
- Public surface:
  - Standalone API: `CinepakDecoder`, `CinepakFrame`, `CinepakPlane`,
    `CinepakPixelFormat`, `CinepakError`, `Result`.
  - Framework API (default-on `registry` feature): `register_codecs`,
    `register`, `__oxideav_entry`. Claims FourCC `CVID`.
- Default-on `registry` cargo feature gates the `oxideav-core` dep so
  image-library consumers can depend on `oxideav-cinepak` with
  `default-features = false` for a no-`oxideav-core` build.

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch and
  is **not** an input for this rebuild.
