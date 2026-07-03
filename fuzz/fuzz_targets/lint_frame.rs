// Panic-free fuzz target for the wire-format conformance linter.
//
// `lint_frame` / `lint_frame_with` promise a total function over
// arbitrary bytes: every input yields a `LintReport` (unparseable
// structure is itself a finding), never a panic, overflow, or OOM.
// The linter walks every wire layer documented in
// `docs/video/cinepak/spec/{01-frame-and-strip,02-codebooks,03-vectors-and-macroblocks}.md`
// — frame header, strip table (y-sentinel geometry), chunk stream,
// codebook payload arithmetic, selective-update group walk, vector
// payload byte balance, and the intra codebook-occupancy check — so
// this target reaches strictly more branch surface than the decoder
// targets (the linter keeps walking where the decoder bails on the
// first error).
//
// All three option shapes are driven per input: default, the
// vintage-player profile, and sequence-start context. Cross-checks:
// the reports must agree on `strips_walked`, the gated profiles can
// only add findings (never remove any), and every issue must render
// through `Display` without panicking.
//
// ## OOM / CPU cap
//
// Lint work is proportional to the input length (per-macroblock walks
// are bounded by payload bytes, not by the declared dimensions), so
// only the sibling targets' defence-in-depth raw-input cap is needed.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::{lint_frame, lint_frame_with, LintOptions};

/// Defence-in-depth raw-input cap, matching the sibling targets.
const MAX_INPUT_LEN: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_LEN {
        return;
    }

    let base = lint_frame(data);
    let vintage = lint_frame_with(data, &LintOptions::new().with_vintage(true));
    let seq = lint_frame_with(data, &LintOptions::new().with_sequence_start(true));

    // The option knobs gate additional rules; they must not change
    // the structural walk or suppress base findings.
    assert_eq!(base.strips_walked(), vintage.strips_walked());
    assert_eq!(base.strips_walked(), seq.strips_walked());
    assert!(vintage.issues().len() >= base.issues().len());
    assert!(seq.issues().len() >= base.issues().len());

    // Every finding renders.
    for issue in base
        .issues()
        .iter()
        .chain(vintage.issues())
        .chain(seq.issues())
    {
        let _ = issue.to_string();
        let _ = issue.rule.spec_ref();
    }
});
