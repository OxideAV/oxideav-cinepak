// Panic-free fuzz target for `cinepak::header::FrameHeader::parse`.
//
// The 10-byte frame header is the entry point for every Cinepak frame:
// flags (1 B) + size (3 B) + width / height (2 B each) + strip-count
// (2 B), per `docs/video/cinepak/spec/01-frame-and-strip.md` §1. The
// parser must reject malformed inputs without panicking or overflowing.
//
// No raster allocation happens here, so the harness can hand the
// parser arbitrarily small or large slices and just observe whether it
// returns `Ok` / `Err` cleanly.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::header::FrameHeader;

fuzz_target!(|data: &[u8]| {
    // Defence-in-depth: cap inputs at 64 KiB so we don't sit in the
    // parser examining megabyte-scale inputs during early corpus growth.
    if data.len() > 64 * 1024 {
        return;
    }
    let _ = FrameHeader::parse(data);
});
