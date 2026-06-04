// Panic-free fuzz target for the round-228 FILM linear-PCM shaping
// helpers and the `FilmAudioFormat::decode_chunk_to_i16` dispatcher.
//
// Per `docs/video/cinepak/spec/05-container-carriage.md` §2 and the
// clean-room wiki notes at
// `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 151..169
// the FILM container interleaves audio samples into the sample-data
// block; each `STAB` row with `sample_info_1 == 0xFFFFFFFF` points at
// a raw-PCM payload whose wire shape is one of four variants:
//
//   - 8-bit mono   — one byte per sample, sign convention per FILM
//     version (twos-complement on Saturn ASCII versions, sign-magnitude
//     on Sega CD / 3DO NULL versions per wiki lines 151 vs 162).
//   - 8-bit stereo — `L0 L1 .. L(n-1) R0 R1 .. R(n-1)` half-chunk
//     layout (wiki lines 156–160), one byte per channel-sample,
//     same sign convention split.
//   - 16-bit mono  — big-endian twos-complement samples (wiki line 153).
//   - 16-bit stereo — half-chunk layout in big-endian twos-complement.
//
// The five round-228 shaping helpers (`pcm_sign_magnitude_to_i8`,
// `pcm_decode_8bit`, `pcm_decode_16be_to_i16`,
// `pcm_deinterleave_stereo_8bit`, `pcm_deinterleave_stereo_16be`)
// plus the `FilmAudioFormat::decode_chunk_to_i16` one-shot wrapper
// receive **attacker-controlled wire bytes** from a STAB-indexed
// audio sample, so each surface needs panic-free fuzz coverage. The
// `film_demuxer_parse` target covers the container parse but exits
// before any audio payload reaches these helpers; the `decode_frame`
// family targets the video bitstream only.
//
// This harness threads one input through all six entry points so a
// single fuzzer corpus contribution that exposes a size-arithmetic
// or sign-extension boundary case improves coverage across the whole
// PCM shaping surface at once. The input layout is:
//
//   - byte 0     — control bits:
//                  bit 0  = sign convention (0 = TwosComplement,
//                           1 = SignMagnitude),
//                  bits 1..3 = channels for the dispatcher path
//                           (masked into `{1, 2, 3, 4}` so the
//                           `None`-on-unsupported-combo branch is
//                           exercised too),
//                  bit 3  = bits_per_sample selector for the
//                           dispatcher (0 = 8, 1 = 16),
//                  bits 4..7 = ignored.
//   - bytes 1..  — payload to feed through each helper.
//
// ## OOM cap
//
// Every helper allocates a `Vec` proportional to the payload length;
// capping the raw input at 64 KiB transitively caps each helper's
// destination buffer at the same 64 KiB, well within any reasonable
// runner memory budget. The dispatcher's worst case (16-bit decode)
// allocates `2 × N` bytes of `i16` for an `N`-byte payload, also
// bounded.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::{
    pcm_decode_16be_to_i16, pcm_decode_8bit, pcm_deinterleave_stereo_16be,
    pcm_deinterleave_stereo_8bit, pcm_sign_magnitude_to_i8, FilmAudioFormat, PcmEndianness,
    PcmSignConvention,
};

/// Defence-in-depth raw-input cap; mirrors the sibling targets.
const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() || data.len() > MAX_INPUT_BYTES {
        return;
    }

    let ctrl = data[0];
    let payload = &data[1..];

    // Sign convention is a one-bit selector; both variants of the
    // 8-bit decode path need coverage (twos-complement is a bitcast,
    // sign-magnitude runs each byte through `pcm_sign_magnitude_to_i8`
    // — a separate arithmetic path).
    let convention = if (ctrl & 0x01) == 0 {
        PcmSignConvention::TwosComplement
    } else {
        PcmSignConvention::SignMagnitude
    };

    // Path A — total-function helper: `pcm_sign_magnitude_to_i8` is
    // documented as defined on every `u8`; drive it across the input
    // bytes to confirm the contract (no panic on `0x00`, `0x80`,
    // boundary high-magnitude values, etc.).
    for &b in payload {
        let _ = pcm_sign_magnitude_to_i8(b);
    }

    // Path B — `pcm_decode_8bit`: the destination must equal the
    // source length per the documented precondition. Feed an exact-
    // length buffer so the success path is exercised; mismatched-
    // length inputs are covered by Path B' below.
    {
        let mut dst = vec![0i8; payload.len()];
        let _ = pcm_decode_8bit(payload, convention, &mut dst);
    }
    // Path B' — `pcm_decode_8bit` with a deliberately-mismatched
    // destination so the `length mismatch` rejection arm runs. The
    // off-by-one is the smallest mutation the parser must reject.
    {
        let mut dst = vec![0i8; payload.len().saturating_add(1)];
        let _ = pcm_decode_8bit(payload, convention, &mut dst);
    }

    // Path C — `pcm_decode_16be_to_i16`: source must be even-length;
    // both even and odd cases must return cleanly. The destination is
    // sized for the documented `src.len() / 2` output count.
    {
        let samples = payload.len() / 2;
        let mut dst = vec![0i16; samples];
        let _ = pcm_decode_16be_to_i16(payload, &mut dst);
    }
    // Path C' — same with the destination off-by-one so the
    // `length mismatch` arm runs even when the source length is
    // even.
    {
        let samples = payload.len() / 2;
        let mut dst = vec![0i16; samples.saturating_add(1)];
        let _ = pcm_decode_16be_to_i16(payload, &mut dst);
    }

    // Path D — `pcm_deinterleave_stereo_8bit`: source must be
    // even-length; destination must equal source length. The
    // helper splits the source at `src.len() / 2` and re-interleaves
    // into `L R L R …` — a per-byte arithmetic path distinct from
    // the per-sample decode helpers above.
    {
        let mut dst = vec![0u8; payload.len()];
        let _ = pcm_deinterleave_stereo_8bit(payload, &mut dst);
    }
    // Path D' — mismatched destination so the `length mismatch` arm
    // runs.
    {
        let mut dst = vec![0u8; payload.len().saturating_add(1)];
        let _ = pcm_deinterleave_stereo_8bit(payload, &mut dst);
    }

    // Path E — `pcm_deinterleave_stereo_16be`: source must be a
    // multiple of 4; destination is `src.len() / 2` samples. The
    // helper splits the source into L / R half-chunks (each half
    // even-length on its own), big-endian-decodes both halves, then
    // re-interleaves — exercises the multi-of-4 boundary plus the
    // big-endian byte ordering plus the channel re-interleave in a
    // single call.
    {
        let samples = payload.len() / 2;
        let mut dst = vec![0i16; samples];
        let _ = pcm_deinterleave_stereo_16be(payload, &mut dst);
    }
    // Path E' — mismatched destination size.
    {
        let samples = payload.len() / 2;
        let mut dst = vec![0i16; samples.saturating_add(1)];
        let _ = pcm_deinterleave_stereo_16be(payload, &mut dst);
    }

    // Path F — `FilmAudioFormat::decode_chunk_to_i16` dispatcher.
    // Use the control byte to pick channels ∈ {1, 2, 3, 4} so the
    // `(8, 1)` / `(8, 2)` / `(16, 1)` / `(16, 2)` documented cells
    // *and* the `None`-on-unsupported-combo arm (channels = 3 or 4)
    // are reached. `bits_per_sample` ∈ {8, 16} per spec — the wiki
    // documents no other widths.
    let channels: u8 = ((ctrl >> 1) & 0x03) + 1; // 1..=4
    let bits_per_sample: u8 = if (ctrl & 0x08) == 0 { 8 } else { 16 };
    let sample_rate_hz: u16 = 22050; // documented Saturn-typical rate
    let fmt = FilmAudioFormat::LinearPcm {
        channels,
        bits_per_sample,
        sample_rate_hz,
        endianness: PcmEndianness::BigEndian,
        sign_convention: convention,
    };
    let _ = fmt.decode_chunk_to_i16(payload);

    // Path G — dispatcher on a non-`LinearPcm` discriminator: must
    // always return `None` regardless of payload shape. Confirms
    // the early-return arm panics-free under fuzzer mutations.
    let _ = FilmAudioFormat::None.decode_chunk_to_i16(payload);
});
