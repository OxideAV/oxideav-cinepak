//! Command-line wire-format conformance linter for raw Cinepak bytes.
//!
//! Reads a file of one or more concatenated standard Cinepak frames
//! (e.g. pre-extracted from an AVI `00dc` chunk, a QuickTime `vide`
//! sample, or a Sega FILM sample — container walking belongs to the
//! container tooling, this driver takes the raw codec bytes) and
//! prints every conformance finding with its rule, severity,
//! strip/chunk location, byte offset, and grounding spec section.
//!
//! Frame splitting uses the 24-bit `frame_length` field, which
//! `docs/video/cinepak/spec/01-frame-and-strip.md` §1.2 documents as
//! the only next-frame pointer in a raw concatenation.
//!
//! ```sh
//! cargo run --example lint_cvid -- [--vintage] [--mid-stream] <file>
//! ```
//!
//! - `--vintage`    also enforce the vintage-player profile
//!   (strip-count ceiling + per-strip codebook-chunk pairing).
//! - `--mid-stream` the file starts mid-sequence (after a keyframe
//!   already decoded elsewhere): suppress the sequence-start rules
//!   for the first frame.
//!
//! Exit status: `0` when every frame is conformant (warnings
//! allowed), `1` when any frame carries an error-severity finding,
//! `2` on usage/IO problems.

use oxideav_cinepak::{lint_frame_with, LintOptions, LintReport};

fn usage() -> ! {
    eprintln!("usage: lint_cvid [--vintage] [--mid-stream] <file.cvid>");
    std::process::exit(2);
}

/// Split concatenated frames on the `frame_length` field (spec 01
/// §1.2). Returns the frame slices plus an optional trailing residue
/// that no well-formed frame header claims.
fn split_frames(bytes: &[u8]) -> (Vec<&[u8]>, Option<&[u8]>) {
    let mut frames = Vec::new();
    let mut rest = bytes;
    while rest.len() >= 10 {
        let frame_length =
            ((rest[1] as usize) << 16) | ((rest[2] as usize) << 8) | (rest[3] as usize);
        if frame_length < 10 || frame_length > rest.len() {
            // Undecidable boundary — hand the remainder to the linter
            // as a single (malformed) frame so the findings surface.
            break;
        }
        frames.push(&rest[..frame_length]);
        rest = &rest[frame_length..];
    }
    if rest.is_empty() {
        (frames, None)
    } else {
        (frames, Some(rest))
    }
}

fn print_report(label: &str, rep: &LintReport) {
    println!(
        "{label}: {} strip(s) walked, {} error(s), {} warning(s){}",
        rep.strips_walked(),
        rep.error_count(),
        rep.warning_count(),
        if rep.is_clean() { " — clean" } else { "" },
    );
    for issue in rep.issues() {
        println!("  {issue}");
    }
}

fn main() {
    let mut vintage = false;
    let mut mid_stream = false;
    let mut path: Option<String> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--vintage" => vintage = true,
            "--mid-stream" => mid_stream = true,
            _ if path.is_none() && !arg.starts_with('-') => path = Some(arg),
            _ => usage(),
        }
    }
    let Some(path) = path else { usage() };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("lint_cvid: cannot read {path}: {e}");
            std::process::exit(2);
        }
    };

    let base = LintOptions::new().with_vintage(vintage);
    let (frames, residue) = split_frames(&bytes);
    let mut any_error = false;

    for (n, frame) in frames.iter().enumerate() {
        let opts = base.with_sequence_start(n == 0 && !mid_stream);
        let rep = lint_frame_with(frame, &opts);
        print_report(&format!("frame {n} ({} bytes)", frame.len()), &rep);
        any_error |= !rep.is_conformant();
    }
    if let Some(residue) = residue {
        let opts = base.with_sequence_start(frames.is_empty() && !mid_stream);
        let rep = lint_frame_with(residue, &opts);
        print_report(
            &format!(
                "trailing residue ({} bytes, no valid frame_length boundary)",
                residue.len()
            ),
            &rep,
        );
        any_error = true;
    }
    if frames.is_empty() && residue.is_none() {
        eprintln!("lint_cvid: {path} is empty");
        std::process::exit(2);
    }

    std::process::exit(if any_error { 1 } else { 0 });
}
