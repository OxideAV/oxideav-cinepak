#![no_main]

//! Decode arbitrary fuzz-supplied bytes through every public Cinepak
//! parser entry point:
//!
//! 1. [`oxideav_cinepak::header::FrameHeader::parse`] — the 10-byte
//!    Cinepak frame header (`spec/01-frame-and-strip.md` §1: 1-byte
//!    flags + 24-bit `frame_length` + `width` + `height` +
//!    `strip_count`). Pure-arithmetic gate; a crash here cascades.
//! 2. [`oxideav_cinepak::header::RawStripHeader::parse`] — the 12-byte
//!    strip header (`spec/01-frame-and-strip.md` §2: `strip_id` +
//!    `strip_size` + the four 16-bit Y/X corners). Drives the
//!    strip-boundary loop in the frame decoder.
//! 3. [`oxideav_cinepak::CinepakDecoder::decode_frame`] — the full
//!    standard-Cinepak frame decode (`spec/02-codebooks.md` for the
//!    chunk taxonomy + V4/V1 entry layout,
//!    `spec/03-vectors-and-macroblocks.md` for the 0x3000 / 0x3100 /
//!    0x3200 vector chunks and the V1 / V4 macroblock expansion rules,
//!    `spec/04-yuv-rgb-matrix.md` for the YUV→RGB matrix). This is the
//!    big attack surface — chunk-walker bounds, codebook
//!    selective-update slot-index maths, vector-chunk skip-bit
//!    extractor, codebook inheritance across strips and frames.
//! 4. [`oxideav_cinepak::CinepakDecoder::decode_deviant_frame`] driven
//!    twice — once with [`oxideav_cinepak::DeviantConfig::saturn`]
//!    (12-byte prefix, `frame_length` short by 8, codebook chunks may
//!    have trailing pad — `Sega_FILM.wiki` lines 125–143) and once
//!    with [`oxideav_cinepak::DeviantConfig::lemmings_3do`] (16-byte
//!    prefix — `Sega_FILM.wiki` line 189). The deviant paths exercise
//!    different prefix arithmetic and a `floor(len / entry_size)`
//!    codebook-pad-tolerant truncation step that the standard path
//!    skips, so they need their own coverage.
//! 5. [`oxideav_cinepak::probe_film`] — the 4-byte `'FILM'` signature
//!    probe. Trivial but it's the entry to the container demux below.
//! 6. [`oxideav_cinepak::FilmDemuxer::parse`] — the Sega FILM / CPK
//!    header walker (`reference/wiki/Sega_FILM.wiki` lines 1–224:
//!    16-byte FILM + variable FDSC + 16-byte STAB header + 16-byte ×
//!    `num_entries` sample records). The `num_entries` field is an
//!    on-wire u32 driving a `Vec::with_capacity` — that needs harness
//!    capping or the fuzzer will surface every multi-GB allocation as
//!    an OOM rather than as a logic bug.
//!
//! The contract under test is purely that every call *returns*: a
//! malformed stream yields `Err(CinepakError::…)`, a well-formed one
//! yields `Ok(…)`, and neither path may panic, abort, integer-overflow
//! (in a debug / ASAN build), or index out of bounds — regardless of
//! how hostile the bytes are. Return values are intentionally
//! discarded (a round-trip oracle would need a trusted encoder of the
//! *same* arbitrary stream, which doesn't exist for the deviant /
//! container paths).
//!
//! # Why the raster cap
//!
//! Once `FrameHeader::parse` accepts a header, `CinepakDecoder::decode_frame`
//! allocates one `width × height × 3` (Rgb24) or `width × height` (Gray8)
//! output buffer per frame — `width` and `height` are 16-bit on the
//! wire so the worst case is roughly `65532 × 65532 × 3 ≈ 12 GiB`
//! (the parser already enforces multiple-of-4 alignment). That's a
//! legitimate resource request, not a decoder bug. Letting the
//! allocator OOM on it would be a false positive that masks the real
//! logic bugs this harness is built to find. The harness therefore
//! rejects declared frames whose total raster exceeds a 16 MiB cap
//! (mirroring what a real demuxer's sanity limits would do) before
//! driving the decode, while still exercising every parse / chunk /
//! codebook / vector / expansion path on inputs up to the cap. The
//! library itself imposes no such cap — it follows the spec, which
//! allows the full u16 range.
//!
//! The Sega FILM `STAB.num_entries` field gets the same treatment —
//! `Vec::with_capacity(num_entries as usize)` against an unbounded u32
//! would surface as OOM rather than as a parser bug, so the harness
//! skips inputs whose declared sample-count would exceed a 1 MiB
//! sample-table cap.

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::header::{FrameHeader, RawStripHeader};
use oxideav_cinepak::{probe_film, CinepakDecoder, DeviantConfig, FilmDemuxer};

/// Upper bound on the declared output raster (16 MiB worst case as
/// Rgb24). Anything larger is a resource request, not a logic path,
/// so the harness skips the allocation-heavy `decode_frame` call but
/// still exercises the header parser on the same input.
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

/// Upper bound on the declared Sega FILM `STAB.num_entries`. Each
/// record is a fixed 16 bytes, so a 1 MiB cap admits ~65 536 records —
/// far larger than any plausible real FILM file's video-sample count.
const MAX_FILM_RECORDS: u64 = (1024 * 1024) / 16;

/// Returns `true` iff the declared frame raster (`width × height × 3`,
/// the Rgb24 worst case — Gray8 frames need a third as much) fits in
/// the harness cap. A header that doesn't parse short-circuits to
/// `true` so the `decode_frame` call below still runs and exercises
/// the parser-reject path.
fn raster_fits(data: &[u8]) -> bool {
    let Ok(hdr) = FrameHeader::parse(data) else {
        return true;
    };
    let total = (hdr.width as u64)
        .checked_mul(hdr.height as u64)
        .and_then(|wh| wh.checked_mul(3));
    matches!(total, Some(n) if n <= MAX_OUTPUT_BYTES)
}

/// Same check, but prefix-aware: for deviant variants there are 2 or
/// 6 extra bytes between the standard 10-byte frame header and the
/// first strip, so the header parser sees `data[..10]` regardless.
fn raster_fits_deviant(data: &[u8]) -> bool {
    raster_fits(data)
}

fuzz_target!(|data: &[u8]| {
    // -----------------------------------------------------------------
    // Pure byte→Result parsers. Each is fully bounded by the input
    // slice — the fuzzer can drive them with zero scaffolding.
    // -----------------------------------------------------------------
    let _ = FrameHeader::parse(data);
    let _ = RawStripHeader::parse(data);
    let _ = probe_film(data);

    // -----------------------------------------------------------------
    // Standard Cinepak frame decode. Cap the declared raster so the
    // fuzzer surfaces logic bugs, not resource requests.
    // -----------------------------------------------------------------
    if raster_fits(data) {
        let mut dec = CinepakDecoder::new();
        let _ = dec.decode_frame(data, None);
    }

    // -----------------------------------------------------------------
    // Sega Saturn / Sega CD deviant decode. 12-byte frame-header
    // prefix + 8-byte-short `frame_length` + codebook chunks may carry
    // trailing pad. Same raster cap.
    // -----------------------------------------------------------------
    if raster_fits_deviant(data) {
        let mut dec = CinepakDecoder::new();
        let _ = dec.decode_deviant_frame(data, None, DeviantConfig::saturn());
    }

    // -----------------------------------------------------------------
    // Lemmings 3DO deviant decode. 16-byte frame-header prefix; the
    // remaining deviations match Saturn. Different prefix arithmetic
    // → different bounds-check paths.
    // -----------------------------------------------------------------
    if raster_fits_deviant(data) {
        let mut dec = CinepakDecoder::new();
        let _ = dec.decode_deviant_frame(data, None, DeviantConfig::lemmings_3do());
    }

    // -----------------------------------------------------------------
    // Sega FILM container demuxer. The `STAB.num_entries` field is an
    // on-wire u32 driving a `Vec::with_capacity` — cap it so the
    // fuzzer surfaces logic bugs, not multi-GB OOM requests. The
    // 16-byte FILM header sits at `data[..16]`; `STAB.num_entries`
    // lives at `data[FILM_HEADER + FDSC_LEN + 12 .. + 16]`. We can't
    // know `FDSC_LEN` without parsing, so the harness uses a coarser
    // approach: pre-screen by parsing through to `num_entries` and
    // skipping the demux if it's too large. A demux that fails before
    // reaching `num_entries` falls through and runs `FilmDemuxer::parse`
    // for the parse-reject coverage.
    // -----------------------------------------------------------------
    if film_pre_screen_ok(data) {
        let _ = FilmDemuxer::parse(data);
    }
});

/// Pre-screen a candidate Sega FILM input: return `true` iff the
/// declared `STAB.num_entries` (a u32 read straight off the wire) fits
/// inside the harness's [`MAX_FILM_RECORDS`] cap, OR if the input
/// doesn't reach `num_entries` cleanly (in which case the demuxer's
/// own error path runs and bails fast without a giant allocation).
fn film_pre_screen_ok(data: &[u8]) -> bool {
    // No FILM magic → demuxer bails on the first 4 bytes, safe to call.
    if data.len() < 20 || !probe_film(data) {
        return true;
    }
    // FDSC chunk length lives at data[20..24]; FDSC starts at offset
    // 16 with `b"FDSC"` + 4-byte length.
    if &data[16..20] != b"FDSC" {
        // FDSC missing — demuxer rejects on its own, no allocation
        // risk.
        return true;
    }
    let fdsc_len = u32::from_be_bytes([data[20], data[21], data[22], data[23]]) as usize;
    let Some(stab_off) = 16usize.checked_add(fdsc_len) else {
        return true;
    };
    let Some(stab_end) = stab_off.checked_add(16) else {
        return true;
    };
    if stab_end > data.len() || &data[stab_off..stab_off + 4] != b"STAB" {
        // Pre-screen didn't find STAB cleanly — let the demuxer's own
        // error path run, it'll bail fast.
        return true;
    }
    let num_entries = u32::from_be_bytes([
        data[stab_off + 12],
        data[stab_off + 13],
        data[stab_off + 14],
        data[stab_off + 15],
    ]);
    u64::from(num_entries) <= MAX_FILM_RECORDS
}
