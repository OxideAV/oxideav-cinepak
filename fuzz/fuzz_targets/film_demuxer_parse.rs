// Panic-free fuzz target for `cinepak::FilmDemuxer::parse`, the Sega
// FILM container parser.
//
// Per `docs/video/cinepak/` wiki notes, a Sega FILM file is:
//   - 16-byte `FILM` box (magic + size + version)
//   - `FDSC` description box (codec FourCC + width / height + frame rate)
//   - `STAB` sample-table box (one 16-byte sample record per A/V sample)
//   - the payload concatenation referenced by the STAB offsets
//
// The harness exercises the structural parse only; it does not invoke
// the Cinepak decoder on the per-sample payload slices (that's covered
// by `decode_frame`).
//
// ## OOM cap
//
// The `STAB` size field is a wire-legal u32 and can claim hundreds of
// MiB of sample-record entries. Cap the input itself at 64 KiB, which
// transitively bounds any `STAB` entry table the parser can allocate
// (256 KiB / 16 B per record = 4096 records ceiling).

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::FilmDemuxer;

fuzz_target!(|data: &[u8]| {
    if data.len() > 64 * 1024 {
        return;
    }
    let _ = FilmDemuxer::parse(data);
});
