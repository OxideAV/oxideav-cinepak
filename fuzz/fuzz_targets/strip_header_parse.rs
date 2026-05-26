// Panic-free fuzz target for `cinepak::header::RawStripHeader::parse`.
//
// Per `docs/video/cinepak/spec/01-frame-and-strip.md` §2 each strip is
// prefixed by a 12-byte header carrying the chunk ID, payload size, and
// the strip's y0 / x0 / y1 / x1 bounding box (where y1 is the strip's
// height relative to the previous strip rather than an absolute
// coordinate). The parser must reject truncated / out-of-range headers
// without panicking.
//
// No raster allocation happens here either.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::header::RawStripHeader;

fuzz_target!(|data: &[u8]| {
    if data.len() > 64 * 1024 {
        return;
    }
    let _ = RawStripHeader::parse(data);
});
