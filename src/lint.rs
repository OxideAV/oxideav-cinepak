//! Wire-format conformance lint for Cinepak frames.
//!
//! The decoder (`decoder.rs`) answers "can these bytes be turned into
//! pixels?"; this module answers the stricter, structured question
//! "do these bytes *conform* to the documented wire format, and if
//! not, which rule is violated, where, and how badly?". It walks the
//! frame → strip → chunk → vector layers read-only and produces a
//! [`LintReport`] of [`LintIssue`]s, each tagged with the violated
//! [`LintRule`], a severity, and the strip / chunk / byte-offset
//! location.
//!
//! Rule provenance — every rule cites its clean-room spec section:
//! - `docs/video/cinepak/spec/01-frame-and-strip.md` — frame header,
//!   strip header, y-sentinel geometry, strip-size accounting.
//! - `docs/video/cinepak/spec/02-codebooks.md` — chunk taxonomy,
//!   per-strip-kind chunk restrictions, codebook payload arithmetic.
//! - `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` — vector
//!   chunk taxonomy, macroblock-count byte balance.
//!
//! Two severities are distinguished:
//! - [`LintSeverity::Error`] — the stream violates a normative
//!   wire-format statement (MUST / "is malformed" / permitted-kind
//!   table). Decoders may still accept some of these (the spec asks
//!   decoders to be lenient in places), but a conformant encoder
//!   never produces them.
//! - [`LintSeverity::Warning`] — the stream deviates from a
//!   documented SHOULD, an encoder convention, or a corpus-wide
//!   observation ("observed in every fixture") that conformant
//!   decoders do not rely on.
//!
//! The linter is best-effort: it reports as many independent issues
//! as it can rather than stopping at the first, and it never panics
//! on arbitrary input.

use crate::codebook::{
    CodebookEntries, PixelMode, StripChunkKind, UpdateStyle, WhichCodebook, CHUNK_HEADER_SIZE,
};
use crate::header::{
    RawStripHeader, StripHeader, FRAME_HEADER_SIZE, STRIP_HEADER_SIZE, STRIP_ID_INTER,
    STRIP_ID_INTRA,
};

/// How severe a lint finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LintSeverity {
    /// Deviates from a documented SHOULD / encoder convention /
    /// corpus observation. Streams decode fine.
    Warning,
    /// Violates a normative wire-format statement. A conformant
    /// encoder never produces this.
    Error,
}

/// The individual conformance rules the linter checks.
///
/// Each variant documents the spec section it is grounded in (see
/// [`LintRule::spec_ref`]). The enum is `non_exhaustive`: later
/// rounds add rules for deeper layers without a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LintRule {
    // ---- frame layer (spec 01 §1) ----
    /// Buffer shorter than the 10-byte frame header.
    FrameHeaderTruncated,
    /// `frame_length` smaller than the 10-byte frame header itself.
    FrameLengthUnderHeader,
    /// `frame_length` exceeds the supplied buffer.
    FrameLengthOverrun,
    /// `frame_length != 10 + Σ strip_size[i]` (spec 01 §1.2 / §3).
    FrameLengthAccounting,
    /// `strip_count == 0`; spec 01 §1 requires ≥ 1.
    StripCountZero,
    /// `width == 0` or `height == 0`.
    FrameDimensionsZero,
    /// Frame width not a multiple of 4 (spec 03 §1: no
    /// fractional-macroblock representation).
    FrameWidthNotMultipleOf4,
    /// Frame height not a multiple of 4 (spec 03 §1).
    FrameHeightNotMultipleOf4,
    /// Upper seven bits of `flags` set; spec 01 §1.1 says the
    /// reference impl SHOULD write zero (decoders ignore them).
    FlagsReservedBitsSet,
    /// `flags` bit 0 advertises codebook inheritance but every strip
    /// is intra-coded; intra strips do not inherit (spec 01 §1.1 +
    /// spec 02 §5.2).
    FlagsInheritanceOnIntraFrame,

    // ---- strip layer (spec 01 §2) ----
    /// Fewer than 12 bytes left where a strip header was declared.
    StripHeaderTruncated,
    /// `strip_size < 12` (the strip header's own size; spec 01 §2).
    StripSizeUnderHeader,
    /// Strip's declared `strip_size` overruns the frame.
    StripOverrunsFrame,
    /// `strip_id` outside the documented `{0x1000, 0x1100}` taxonomy
    /// (spec 01 §2.1).
    UnknownStripId,
    /// Intra and inter strips mixed within one frame. Spec 01 §2.1:
    /// all strips of a frame share the same kind in every observed
    /// stream; mixed-type frames are undocumented (decoders fall back
    /// to per-strip dispatch).
    MixedStripKinds,
    /// Resolved strip rectangle has `y_bottom ≤ y_top` or
    /// `x_bottom ≤ x_top` (zero or negative extent).
    StripEmptyRect,
    /// Strip height (after the §2.2 sentinel resolution) not a
    /// multiple of 4 (spec 03 §1 MUST).
    StripHeightNotMultipleOf4,
    /// Strip width not a multiple of 4 (spec 03 §1 MUST).
    StripWidthNotMultipleOf4,
    /// Strip rectangle extends beyond the frame's coded dimensions.
    StripOutsideFrame,
    /// Strip does not span the full frame width. Spec 01 §2.2: strips
    /// were observed spanning `[0, frame_width)` in every fixture.
    StripNotFullWidth,
    /// Strips do not tile the frame's vertical extent contiguously
    /// from 0 to `height` (gap, overlap, or short coverage). Every
    /// observed stream tiles exactly (spec 01 §2.2 tables O3/O5).
    StripCoverageIrregular,

    // ---- chunk layer (spec 02) ----
    /// Fewer than 4 bytes left where a chunk header was declared.
    /// Also covers the spec 02 §1 `Σ chunk_size == strip_size − 12`
    /// accounting: leftover strip bytes that no chunk claims are
    /// reported here (1–3 bytes) or as an unrecognised chunk.
    ChunkHeaderTruncated,
    /// `chunk_size < 4` (the chunk header's own size; spec 02 §1).
    ChunkSizeUnderHeader,
    /// Chunk's declared `chunk_size` overruns the strip payload
    /// (spec 02 §1 accounting).
    ChunkOverrunsStrip,
    /// `chunk_id` outside the documented codebook (`0x20xx`–`0x27xx`,
    /// low byte zero) and vector (`0x3000`/`0x3100`/`0x3200`)
    /// taxonomies (spec 02 §2.1 + spec 03 §2.1).
    UnknownChunkId,
    /// Selective-update codebook chunk (`0x21xx`/`0x23xx`/`0x25xx`/
    /// `0x27xx`) on an intra strip; spec 02 §2.3: only the
    /// full-replacement family is permitted (there is no prior
    /// codebook to update against).
    SelectiveUpdateOnIntraStrip,
    /// Header-only full-replacement chunk (`chunk_size = 4`) on an
    /// intra strip: wire-legal but vacuously useless (spec 02 §3.4 —
    /// there is no prior codebook to reuse).
    HeaderOnlyCodebookOnIntraStrip,
    /// Full-replacement codebook payload not a multiple of the entry
    /// size (6 B for 12-bit YUV, 4 B for grayscale; spec 02 §3.1).
    CodebookPayloadMisaligned,
    /// Full-replacement codebook chunk carrying more than 256 entries
    /// (spec 02 §3.1: 8-bit vector indices cap the codebook at 256).
    CodebookTooManyEntries,
    /// Selective-update payload malformed: truncated flag word or
    /// entry, or more than 8 update groups / 256 slots addressed
    /// (spec 02 §4.2–§4.3).
    SelectiveUpdatePayloadMalformed,
    /// Codebook chunks of both pixel modes (12-bit YUV `0x20xx`/
    /// `0x22xx` and grayscale `0x24xx`/`0x26xx`) within one frame;
    /// spec 04 §4: a conformant stream MUST NOT mix the two flavours
    /// within the same frame.
    MixedPixelModes,
    /// A V1 codebook chunk precedes a V4 codebook chunk within the
    /// strip; spec 02 §2.2 documents strict V4-then-V1 emission order
    /// (vintage MacOS players require it; modern decoders don't).
    CodebookOrderNotV4ThenV1,
    /// Two codebook chunks addressing the same flavour (V4 or V1)
    /// within one strip. Spec 02 §2.2 enumerates one chunk per
    /// flavour; no observed stream repeats a flavour.
    DuplicateCodebookChunk,
    /// A chunk appears after the strip's vector chunk; spec 03 §2:
    /// each strip carries exactly one vector chunk **after** its
    /// codebook chunks.
    ChunkAfterVectorChunk,
}

impl LintRule {
    /// The severity this rule reports at.
    pub fn severity(self) -> LintSeverity {
        match self {
            LintRule::FlagsReservedBitsSet
            | LintRule::FlagsInheritanceOnIntraFrame
            | LintRule::MixedStripKinds
            | LintRule::StripNotFullWidth
            | LintRule::StripCoverageIrregular
            | LintRule::HeaderOnlyCodebookOnIntraStrip
            | LintRule::CodebookOrderNotV4ThenV1
            | LintRule::DuplicateCodebookChunk => LintSeverity::Warning,
            _ => LintSeverity::Error,
        }
    }

    /// The clean-room spec section this rule is grounded in
    /// (path relative to `docs/video/cinepak/spec/`).
    pub fn spec_ref(self) -> &'static str {
        match self {
            LintRule::FrameHeaderTruncated
            | LintRule::FrameLengthUnderHeader
            | LintRule::FrameLengthOverrun
            | LintRule::StripCountZero
            | LintRule::FrameDimensionsZero => "01-frame-and-strip.md §1",
            LintRule::FlagsReservedBitsSet | LintRule::FlagsInheritanceOnIntraFrame => {
                "01-frame-and-strip.md §1.1"
            }
            LintRule::FrameLengthAccounting => "01-frame-and-strip.md §1.2",
            LintRule::StripHeaderTruncated
            | LintRule::StripSizeUnderHeader
            | LintRule::StripOverrunsFrame => "01-frame-and-strip.md §2",
            LintRule::UnknownStripId | LintRule::MixedStripKinds => "01-frame-and-strip.md §2.1",
            LintRule::StripEmptyRect
            | LintRule::StripOutsideFrame
            | LintRule::StripNotFullWidth
            | LintRule::StripCoverageIrregular => "01-frame-and-strip.md §2.2",
            LintRule::FrameWidthNotMultipleOf4
            | LintRule::FrameHeightNotMultipleOf4
            | LintRule::StripHeightNotMultipleOf4
            | LintRule::StripWidthNotMultipleOf4 => "03-vectors-and-macroblocks.md §1",
            LintRule::ChunkHeaderTruncated
            | LintRule::ChunkSizeUnderHeader
            | LintRule::ChunkOverrunsStrip => "02-codebooks.md §1",
            LintRule::UnknownChunkId => "02-codebooks.md §2.1 + 03-vectors-and-macroblocks.md §2.1",
            LintRule::SelectiveUpdateOnIntraStrip => "02-codebooks.md §2.3",
            LintRule::HeaderOnlyCodebookOnIntraStrip => "02-codebooks.md §3.4",
            LintRule::CodebookPayloadMisaligned | LintRule::CodebookTooManyEntries => {
                "02-codebooks.md §3.1"
            }
            LintRule::SelectiveUpdatePayloadMalformed => "02-codebooks.md §4.2–§4.3",
            LintRule::MixedPixelModes => "04-yuv-rgb-matrix.md §4",
            LintRule::CodebookOrderNotV4ThenV1 | LintRule::DuplicateCodebookChunk => {
                "02-codebooks.md §2.2"
            }
            LintRule::ChunkAfterVectorChunk => "03-vectors-and-macroblocks.md §2",
        }
    }
}

/// One lint finding: a violated rule plus its location and a
/// human-readable message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LintIssue {
    /// The violated rule.
    pub rule: LintRule,
    /// Convenience copy of `rule.severity()`.
    pub severity: LintSeverity,
    /// 0-based strip index the issue belongs to, when strip-scoped.
    pub strip: Option<u16>,
    /// 0-based chunk index within the strip, when chunk-scoped.
    pub chunk: Option<u16>,
    /// Byte offset within the linted frame buffer where the issue was
    /// detected (best effort; points at the enclosing structure).
    pub offset: usize,
    /// Human-readable description with the concrete offending values.
    pub message: String,
}

impl core::fmt::Display for LintIssue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let sev = match self.severity {
            LintSeverity::Error => "error",
            LintSeverity::Warning => "warning",
        };
        write!(f, "{sev}[{:?}]", self.rule)?;
        if let Some(s) = self.strip {
            write!(f, " strip {s}")?;
        }
        if let Some(c) = self.chunk {
            write!(f, " chunk {c}")?;
        }
        write!(
            f,
            " @0x{:04x}: {} (spec {})",
            self.offset,
            self.message,
            self.rule.spec_ref()
        )
    }
}

/// Lint configuration knobs. Defaults check the modern wire format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LintOptions {}

impl LintOptions {
    /// The default option set.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The outcome of linting one frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LintReport {
    issues: Vec<LintIssue>,
    /// Number of strips whose headers were successfully walked.
    strips_walked: u16,
}

impl LintReport {
    /// All findings, in detection order (frame layer first, then
    /// per-strip in wire order).
    pub fn issues(&self) -> &[LintIssue] {
        &self.issues
    }

    /// Number of `Error`-severity findings.
    pub fn error_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == LintSeverity::Error)
            .count()
    }

    /// Number of `Warning`-severity findings.
    pub fn warning_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == LintSeverity::Warning)
            .count()
    }

    /// `true` when the frame carries no `Error`-severity findings
    /// (warnings allowed).
    pub fn is_conformant(&self) -> bool {
        self.error_count() == 0
    }

    /// `true` when the frame carries no findings at all.
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    /// Number of strip headers successfully walked (≤ the header's
    /// declared `strip_count` when the walk aborted early).
    pub fn strips_walked(&self) -> u16 {
        self.strips_walked
    }

    fn push(&mut self, rule: LintRule, strip: Option<u16>, offset: usize, message: String) {
        self.issues.push(LintIssue {
            rule,
            severity: rule.severity(),
            strip,
            chunk: None,
            offset,
            message,
        });
    }

    fn push_chunk(
        &mut self,
        rule: LintRule,
        strip: u16,
        chunk: u16,
        offset: usize,
        message: String,
    ) {
        self.issues.push(LintIssue {
            rule,
            severity: rule.severity(),
            strip: Some(strip),
            chunk: Some(chunk),
            offset,
            message,
        });
    }
}

/// Lint one standard (non-deviant) Cinepak frame with default
/// options. See [`lint_frame_with`].
pub fn lint_frame(bytes: &[u8]) -> LintReport {
    lint_frame_with(bytes, &LintOptions::default())
}

/// Frame-scoped state threaded through the per-strip chunk walks.
#[derive(Default)]
struct FrameChunkState {
    /// Pixel mode established by the first codebook chunk of the
    /// frame, with the strip index that set it (spec 04 §4: implicit
    /// per-frame flavour).
    mode: Option<(PixelMode, u16)>,
    /// Set once [`LintRule::MixedPixelModes`] has been reported so a
    /// long mixed frame doesn't flood the report.
    mixed_reported: bool,
}

/// Walk one strip's chunk stream (spec 02 §1 layout) and report
/// chunk-layer conformance issues.
///
/// `is_intra` is `Some(kind)` when the strip id classified, `None`
/// for an unknown strip id (kind-dependent rules are skipped).
/// `base_off` is the byte offset of `payload` within the frame
/// buffer, for issue locations.
fn lint_strip_chunks(
    rep: &mut LintReport,
    strip: u16,
    is_intra: Option<bool>,
    payload: &[u8],
    base_off: usize,
    frame_state: &mut FrameChunkState,
) {
    let mut pos = 0usize;
    let mut chunk_idx: u16 = 0;
    let mut saw_v4 = false;
    let mut saw_v1 = false;
    let mut vector_chunk_at: Option<u16> = None;

    while pos < payload.len() {
        let off = base_off + pos;
        if pos + CHUNK_HEADER_SIZE > payload.len() {
            rep.push_chunk(
                LintRule::ChunkHeaderTruncated,
                strip,
                chunk_idx,
                off,
                format!(
                    "{} unclaimed strip byte(s); a chunk header needs 4 (Σ chunk_size must equal strip_size − 12)",
                    payload.len() - pos
                ),
            );
            return;
        }
        let id = u16::from_be_bytes([payload[pos], payload[pos + 1]]);
        let size = u16::from_be_bytes([payload[pos + 2], payload[pos + 3]]) as usize;
        if size < CHUNK_HEADER_SIZE {
            rep.push_chunk(
                LintRule::ChunkSizeUnderHeader,
                strip,
                chunk_idx,
                off + 2,
                format!("chunk_size {size} < 4 (its own header)"),
            );
            // Cannot advance reliably past a chunk smaller than its
            // own header.
            return;
        }
        let mut end = pos + size;
        let mut truncated = false;
        if end > payload.len() {
            rep.push_chunk(
                LintRule::ChunkOverrunsStrip,
                strip,
                chunk_idx,
                off + 2,
                format!(
                    "chunk_size {size} overruns the strip payload by {} bytes",
                    end - payload.len()
                ),
            );
            end = payload.len();
            truncated = true;
        }
        let body = &payload[pos + CHUNK_HEADER_SIZE..end];

        match StripChunkKind::from_id(id) {
            None => {
                rep.push_chunk(
                    LintRule::UnknownChunkId,
                    strip,
                    chunk_idx,
                    off,
                    format!("chunk_id 0x{id:04x} is neither a codebook nor a vector chunk code"),
                );
            }
            Some(StripChunkKind::Codebook(kind)) => {
                if let Some(v_at) = vector_chunk_at {
                    rep.push_chunk(
                        LintRule::ChunkAfterVectorChunk,
                        strip,
                        chunk_idx,
                        off,
                        format!(
                            "codebook chunk 0x{id:04x} appears after the vector chunk (chunk {v_at})"
                        ),
                    );
                }
                // Pixel-mode consistency across the whole frame.
                match frame_state.mode {
                    None => frame_state.mode = Some((kind.mode, strip)),
                    Some((m, first_strip)) => {
                        if m != kind.mode && !frame_state.mixed_reported {
                            frame_state.mixed_reported = true;
                            rep.push_chunk(
                                LintRule::MixedPixelModes,
                                strip,
                                chunk_idx,
                                off,
                                format!(
                                    "chunk 0x{id:04x} is {:?} but strip {first_strip} established {m:?}",
                                    kind.mode
                                ),
                            );
                        }
                    }
                }
                // Per-flavour presence + ordering (spec 02 §2.2).
                match kind.which {
                    WhichCodebook::V4 => {
                        if saw_v4 {
                            rep.push_chunk(
                                LintRule::DuplicateCodebookChunk,
                                strip,
                                chunk_idx,
                                off,
                                "second V4 codebook chunk in one strip".into(),
                            );
                        }
                        if saw_v1 && !saw_v4 {
                            rep.push_chunk(
                                LintRule::CodebookOrderNotV4ThenV1,
                                strip,
                                chunk_idx,
                                off,
                                "V4 codebook chunk arrives after a V1 codebook chunk".into(),
                            );
                        }
                        saw_v4 = true;
                    }
                    WhichCodebook::V1 => {
                        if saw_v1 {
                            rep.push_chunk(
                                LintRule::DuplicateCodebookChunk,
                                strip,
                                chunk_idx,
                                off,
                                "second V1 codebook chunk in one strip".into(),
                            );
                        }
                        saw_v1 = true;
                    }
                }
                match kind.style {
                    UpdateStyle::Full => {
                        if !truncated {
                            let entry = kind.mode.entry_size();
                            if body.len() % entry != 0 {
                                rep.push_chunk(
                                    LintRule::CodebookPayloadMisaligned,
                                    strip,
                                    chunk_idx,
                                    off + 2,
                                    format!(
                                        "payload {} bytes is not a multiple of the {entry}-byte entry size",
                                        body.len()
                                    ),
                                );
                            } else if body.len() / entry > 256 {
                                rep.push_chunk(
                                    LintRule::CodebookTooManyEntries,
                                    strip,
                                    chunk_idx,
                                    off + 2,
                                    format!(
                                        "{} entries exceed the 256-entry codebook",
                                        body.len() / entry
                                    ),
                                );
                            }
                            if is_intra == Some(true) && body.is_empty() {
                                rep.push_chunk(
                                    LintRule::HeaderOnlyCodebookOnIntraStrip,
                                    strip,
                                    chunk_idx,
                                    off,
                                    format!(
                                        "header-only chunk 0x{id:04x} on an intra strip has no prior codebook to reuse"
                                    ),
                                );
                            }
                        }
                    }
                    UpdateStyle::Selective => {
                        if is_intra == Some(true) {
                            rep.push_chunk(
                                LintRule::SelectiveUpdateOnIntraStrip,
                                strip,
                                chunk_idx,
                                off,
                                format!("selective-update chunk 0x{id:04x} on an intra strip"),
                            );
                        }
                        if !truncated {
                            match CodebookEntries::new(kind, body) {
                                Err(e) => rep.push_chunk(
                                    LintRule::SelectiveUpdatePayloadMalformed,
                                    strip,
                                    chunk_idx,
                                    off + CHUNK_HEADER_SIZE,
                                    e.to_string(),
                                ),
                                Ok(entries) => {
                                    for r in entries {
                                        if let Err(e) = r {
                                            rep.push_chunk(
                                                LintRule::SelectiveUpdatePayloadMalformed,
                                                strip,
                                                chunk_idx,
                                                off + CHUNK_HEADER_SIZE,
                                                e.to_string(),
                                            );
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Some(StripChunkKind::Vector(_)) => {
                if let Some(v_at) = vector_chunk_at {
                    rep.push_chunk(
                        LintRule::ChunkAfterVectorChunk,
                        strip,
                        chunk_idx,
                        off,
                        format!(
                            "vector chunk 0x{id:04x} appears after the vector chunk (chunk {v_at})"
                        ),
                    );
                } else {
                    vector_chunk_at = Some(chunk_idx);
                }
            }
        }

        if truncated {
            return;
        }
        pos = end;
        chunk_idx += 1;
    }
}

/// Lint one standard (non-deviant) Cinepak frame.
///
/// Never fails and never panics: unparseable structure is itself a
/// lint finding. The walk is best-effort — a frame-layer error that
/// makes the strip table unreachable stops the walk after reporting,
/// while localised strip issues let the walk continue to the next
/// strip.
pub fn lint_frame_with(bytes: &[u8], opts: &LintOptions) -> LintReport {
    let _ = opts; // No option-dependent rules at this layer yet.
    let mut rep = LintReport::default();

    // ---- frame header (spec 01 §1) ----
    if bytes.len() < FRAME_HEADER_SIZE {
        rep.push(
            LintRule::FrameHeaderTruncated,
            None,
            0,
            format!(
                "buffer holds {} bytes; the frame header needs {}",
                bytes.len(),
                FRAME_HEADER_SIZE
            ),
        );
        return rep;
    }
    let flags = bytes[0];
    let frame_length =
        ((bytes[1] as usize) << 16) | ((bytes[2] as usize) << 8) | (bytes[3] as usize);
    let width = u16::from_be_bytes([bytes[4], bytes[5]]);
    let height = u16::from_be_bytes([bytes[6], bytes[7]]);
    let strip_count = u16::from_be_bytes([bytes[8], bytes[9]]);

    if flags & 0xfe != 0 {
        rep.push(
            LintRule::FlagsReservedBitsSet,
            None,
            0,
            format!("flags = 0x{flags:02x}; upper seven bits should be zero"),
        );
    }
    if width == 0 || height == 0 {
        rep.push(
            LintRule::FrameDimensionsZero,
            None,
            4,
            format!("coded dimensions {width}x{height}"),
        );
    }
    if width % 4 != 0 {
        rep.push(
            LintRule::FrameWidthNotMultipleOf4,
            None,
            4,
            format!("width {width} is not a multiple of 4"),
        );
    }
    if height % 4 != 0 {
        rep.push(
            LintRule::FrameHeightNotMultipleOf4,
            None,
            6,
            format!("height {height} is not a multiple of 4"),
        );
    }
    if strip_count == 0 {
        rep.push(
            LintRule::StripCountZero,
            None,
            8,
            "strip_count = 0; spec requires ≥ 1".into(),
        );
    }
    if frame_length < FRAME_HEADER_SIZE {
        rep.push(
            LintRule::FrameLengthUnderHeader,
            None,
            1,
            format!("frame_length {frame_length} < 10-byte frame header"),
        );
        // The declared frame is smaller than its own header — there
        // is no strip table to walk.
        return rep;
    }
    // Walk within min(frame_length, buffer): an overrunning
    // frame_length is reported but the present bytes are still
    // linted best-effort.
    let walk_len = if frame_length > bytes.len() {
        rep.push(
            LintRule::FrameLengthOverrun,
            None,
            1,
            format!(
                "frame_length {frame_length} exceeds the {}-byte buffer",
                bytes.len()
            ),
        );
        bytes.len()
    } else {
        frame_length
    };

    // ---- strip walk (spec 01 §2 + §3 decoder algorithm) ----
    let mut cursor = FRAME_HEADER_SIZE;
    let mut prev_y_bottom: u32 = 0;
    let mut sum_strip_sizes: usize = 0;
    let mut saw_intra = false;
    let mut saw_inter = false;
    let mut coverage_regular = true;
    let mut walk_truncated = false;
    let mut frame_chunk_state = FrameChunkState::default();

    for i in 0..strip_count {
        if cursor + STRIP_HEADER_SIZE > walk_len {
            rep.push(
                LintRule::StripHeaderTruncated,
                Some(i),
                cursor,
                format!(
                    "strip {i} header needs 12 bytes; {} remain in the frame",
                    walk_len - cursor
                ),
            );
            walk_truncated = true;
            break;
        }
        let strip_off = cursor;
        let raw = RawStripHeader {
            strip_id: u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]),
            strip_size: u16::from_be_bytes([bytes[cursor + 2], bytes[cursor + 3]]),
            y_top: u16::from_be_bytes([bytes[cursor + 4], bytes[cursor + 5]]),
            x_top: u16::from_be_bytes([bytes[cursor + 6], bytes[cursor + 7]]),
            y_bottom: u16::from_be_bytes([bytes[cursor + 8], bytes[cursor + 9]]),
            x_bottom: u16::from_be_bytes([bytes[cursor + 10], bytes[cursor + 11]]),
        };

        match raw.strip_id {
            STRIP_ID_INTRA => saw_intra = true,
            STRIP_ID_INTER => saw_inter = true,
            other => rep.push(
                LintRule::UnknownStripId,
                Some(i),
                strip_off,
                format!("strip_id 0x{other:04x}; only 0x1000 (intra) and 0x1100 (inter) exist"),
            ),
        }
        if (raw.strip_size as usize) < STRIP_HEADER_SIZE {
            rep.push(
                LintRule::StripSizeUnderHeader,
                Some(i),
                strip_off + 2,
                format!("strip_size {} < 12 (its own header)", raw.strip_size),
            );
            // Cannot advance reliably past a strip smaller than its
            // own header.
            walk_truncated = true;
            break;
        }
        let mut strip_end = cursor + raw.strip_size as usize;
        if strip_end > walk_len {
            rep.push(
                LintRule::StripOverrunsFrame,
                Some(i),
                strip_off + 2,
                format!(
                    "strip_size {} overruns the frame by {} bytes",
                    raw.strip_size,
                    strip_end - walk_len
                ),
            );
            strip_end = walk_len;
            walk_truncated = true;
        }
        sum_strip_sizes += raw.strip_size as usize;

        // Geometry (spec 01 §2.2 sentinel + spec 03 §1 grid rules).
        let hdr = StripHeader::resolve(raw, i == 0, prev_y_bottom);
        let (h, w) = (hdr.height(), hdr.width());
        if hdr.actual_y_bottom <= hdr.actual_y_top || raw.x_bottom <= raw.x_top {
            rep.push(
                LintRule::StripEmptyRect,
                Some(i),
                strip_off + 4,
                format!(
                    "resolved rect y [{}, {}) x [{}, {}) has zero or negative extent",
                    hdr.actual_y_top, hdr.actual_y_bottom, raw.x_top, raw.x_bottom
                ),
            );
        } else {
            if h % 4 != 0 {
                rep.push(
                    LintRule::StripHeightNotMultipleOf4,
                    Some(i),
                    strip_off + 8,
                    format!("strip height {h} is not a multiple of 4"),
                );
            }
            if w % 4 != 0 {
                rep.push(
                    LintRule::StripWidthNotMultipleOf4,
                    Some(i),
                    strip_off + 10,
                    format!("strip width {w} is not a multiple of 4"),
                );
            }
            if hdr.actual_y_bottom > u32::from(height) || u32::from(raw.x_bottom) > u32::from(width)
            {
                rep.push(
                    LintRule::StripOutsideFrame,
                    Some(i),
                    strip_off + 4,
                    format!(
                        "rect y [{}, {}) x [{}, {}) exceeds the {width}x{height} frame",
                        hdr.actual_y_top, hdr.actual_y_bottom, raw.x_top, raw.x_bottom
                    ),
                );
            }
            if raw.x_top != 0 || raw.x_bottom != width {
                rep.push(
                    LintRule::StripNotFullWidth,
                    Some(i),
                    strip_off + 6,
                    format!(
                        "x [{}, {}) does not span the full width {width}",
                        raw.x_top, raw.x_bottom
                    ),
                );
            }
        }
        // Vertical tiling: each strip starts where the previous ended.
        if hdr.actual_y_top != prev_y_bottom {
            coverage_regular = false;
        }
        prev_y_bottom = hdr.actual_y_bottom;

        // ---- chunk layer (spec 02 §1) ----
        let is_intra = match raw.strip_id {
            STRIP_ID_INTRA => Some(true),
            STRIP_ID_INTER => Some(false),
            _ => None,
        };
        let payload_start = strip_off + STRIP_HEADER_SIZE;
        if payload_start <= strip_end {
            lint_strip_chunks(
                &mut rep,
                i,
                is_intra,
                &bytes[payload_start..strip_end],
                payload_start,
                &mut frame_chunk_state,
            );
        }

        rep.strips_walked += 1;
        cursor = strip_end;
        if walk_truncated {
            break;
        }
    }

    // ---- frame-level cross-checks ----
    if !walk_truncated && rep.strips_walked == strip_count && strip_count > 0 {
        let expected = FRAME_HEADER_SIZE + sum_strip_sizes;
        if expected != frame_length {
            rep.push(
                LintRule::FrameLengthAccounting,
                None,
                1,
                format!("frame_length {frame_length} != 10 + Σ strip_size = {expected}"),
            );
        }
        if prev_y_bottom != u32::from(height) || !coverage_regular {
            rep.push(
                LintRule::StripCoverageIrregular,
                None,
                FRAME_HEADER_SIZE,
                format!(
                    "strips do not tile y [0, {height}) contiguously (last bottom {prev_y_bottom})"
                ),
            );
        }
    }
    if saw_intra && saw_inter {
        rep.push(
            LintRule::MixedStripKinds,
            None,
            FRAME_HEADER_SIZE,
            "frame mixes intra (0x1000) and inter (0x1100) strips".into(),
        );
    }
    if flags & 0x01 != 0 && saw_intra && !saw_inter {
        rep.push(
            LintRule::FlagsInheritanceOnIntraFrame,
            None,
            0,
            "flags bit 0 advertises codebook inheritance but every strip is intra-coded".into(),
        );
    }

    rep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{encode_rgb24, EncoderOptions};

    /// Build the spec 01 §1.1 `O1`-shaped single-strip frame body with
    /// a synthetic payload of `payload_len` zero bytes (not chunk-
    /// conformant — frame/strip layer tests only).
    fn frame_one_strip(payload_len: usize) -> Vec<u8> {
        let strip_size = STRIP_HEADER_SIZE + payload_len;
        let frame_length = FRAME_HEADER_SIZE + strip_size;
        let mut v = Vec::new();
        v.push(0x00); // flags
        v.extend_from_slice(&[
            ((frame_length >> 16) & 0xff) as u8,
            ((frame_length >> 8) & 0xff) as u8,
            (frame_length & 0xff) as u8,
        ]);
        v.extend_from_slice(&16u16.to_be_bytes()); // width
        v.extend_from_slice(&8u16.to_be_bytes()); // height
        v.extend_from_slice(&1u16.to_be_bytes()); // strip_count
        v.extend_from_slice(&0x1000u16.to_be_bytes()); // intra strip
        v.extend_from_slice(&(strip_size as u16).to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes()); // y_top
        v.extend_from_slice(&0u16.to_be_bytes()); // x_top
        v.extend_from_slice(&8u16.to_be_bytes()); // y_bottom
        v.extend_from_slice(&16u16.to_be_bytes()); // x_bottom
        v.extend_from_slice(&vec![0u8; payload_len]);
        v
    }

    fn has_rule(rep: &LintReport, rule: LintRule) -> bool {
        rep.issues().iter().any(|i| i.rule == rule)
    }

    #[test]
    fn encoder_output_is_frame_and_strip_clean() {
        // A real encoded frame must carry no frame/strip-layer issues.
        let (w, h) = (32u32, 16u32);
        let rgb = vec![128u8; (w * h * 3) as usize];
        let bytes = encode_rgb24(&rgb, w, h, EncoderOptions::default()).unwrap();
        let rep = lint_frame(&bytes);
        assert!(rep.is_clean(), "unexpected issues: {:?}", rep.issues());
        assert_eq!(rep.strips_walked(), 1);
    }

    #[test]
    fn multi_strip_encoder_output_is_clean() {
        let (w, h) = (16u32, 24u32);
        let rgb: Vec<u8> = (0..(w * h * 3)).map(|i| (i % 251) as u8).collect();
        let opts = EncoderOptions {
            strip_count: 3,
            ..EncoderOptions::default()
        };
        let bytes = encode_rgb24(&rgb, w, h, opts).unwrap();
        let rep = lint_frame(&bytes);
        assert!(rep.is_clean(), "unexpected issues: {:?}", rep.issues());
        assert_eq!(rep.strips_walked(), 3);
    }

    #[test]
    fn truncated_header_is_reported() {
        let rep = lint_frame(&[0u8; 5]);
        assert!(has_rule(&rep, LintRule::FrameHeaderTruncated));
        assert!(!rep.is_conformant());
    }

    #[test]
    fn reserved_flag_bits_warn() {
        let mut f = frame_one_strip(0);
        f[0] = 0x80;
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::FlagsReservedBitsSet));
        // Warning only — the frame stays conformant.
        assert!(rep.is_conformant());
        assert_eq!(rep.warning_count(), 1);
    }

    #[test]
    fn zero_strip_count_is_error() {
        let mut f = frame_one_strip(0);
        f[8..10].copy_from_slice(&0u16.to_be_bytes());
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::StripCountZero));
    }

    #[test]
    fn non_multiple_of_4_dims_are_errors() {
        let mut f = frame_one_strip(0);
        f[4..6].copy_from_slice(&15u16.to_be_bytes());
        f[6..8].copy_from_slice(&9u16.to_be_bytes());
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::FrameWidthNotMultipleOf4));
        assert!(has_rule(&rep, LintRule::FrameHeightNotMultipleOf4));
    }

    #[test]
    fn frame_length_overrun_is_reported_but_walk_continues() {
        let mut f = frame_one_strip(4);
        let declared = f.len() + 100;
        f[1] = ((declared >> 16) & 0xff) as u8;
        f[2] = ((declared >> 8) & 0xff) as u8;
        f[3] = (declared & 0xff) as u8;
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::FrameLengthOverrun));
        // The single strip present is still walked.
        assert_eq!(rep.strips_walked(), 1);
    }

    #[test]
    fn frame_length_under_header_stops_walk() {
        let mut f = frame_one_strip(0);
        f[1] = 0;
        f[2] = 0;
        f[3] = 4;
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::FrameLengthUnderHeader));
        assert_eq!(rep.strips_walked(), 0);
    }

    #[test]
    fn frame_length_accounting_mismatch_is_error() {
        // Declared frame_length includes 2 extra trailing bytes not
        // covered by any strip.
        let mut f = frame_one_strip(0);
        f.extend_from_slice(&[0, 0]);
        let declared = f.len();
        f[1] = ((declared >> 16) & 0xff) as u8;
        f[2] = ((declared >> 8) & 0xff) as u8;
        f[3] = (declared & 0xff) as u8;
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::FrameLengthAccounting));
    }

    #[test]
    fn unknown_strip_id_is_error_and_walk_continues() {
        let mut f = frame_one_strip(0);
        f[10..12].copy_from_slice(&0x1200u16.to_be_bytes());
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::UnknownStripId));
        assert_eq!(rep.strips_walked(), 1);
    }

    #[test]
    fn strip_size_under_header_is_error() {
        let mut f = frame_one_strip(0);
        f[12..14].copy_from_slice(&4u16.to_be_bytes());
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::StripSizeUnderHeader));
    }

    #[test]
    fn strip_overrun_is_error() {
        let mut f = frame_one_strip(0);
        f[12..14].copy_from_slice(&200u16.to_be_bytes());
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::StripOverrunsFrame));
    }

    #[test]
    fn strip_header_truncation_is_error() {
        // Two declared strips but bytes for only one.
        let mut f = frame_one_strip(0);
        f[8..10].copy_from_slice(&2u16.to_be_bytes());
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::StripHeaderTruncated));
        assert_eq!(rep.strips_walked(), 1);
    }

    #[test]
    fn sentinel_rule_multi_strip_geometry_is_clean() {
        // Two strips, second in sentinel form (y_top = 0, y_bottom =
        // height), per spec 01 §2.2 observation O3.
        let strip_size = (STRIP_HEADER_SIZE as u16).to_be_bytes();
        let mut f = Vec::new();
        let frame_length = FRAME_HEADER_SIZE + 2 * STRIP_HEADER_SIZE;
        f.push(0x00);
        f.extend_from_slice(&[0, 0, frame_length as u8]);
        f.extend_from_slice(&16u16.to_be_bytes());
        f.extend_from_slice(&16u16.to_be_bytes());
        f.extend_from_slice(&2u16.to_be_bytes());
        for _ in 0..2 {
            f.extend_from_slice(&0x1000u16.to_be_bytes());
            f.extend_from_slice(&strip_size);
            f.extend_from_slice(&0u16.to_be_bytes()); // y_top sentinel
            f.extend_from_slice(&0u16.to_be_bytes());
            f.extend_from_slice(&8u16.to_be_bytes()); // height 8
            f.extend_from_slice(&16u16.to_be_bytes());
        }
        let rep = lint_frame(&f);
        assert!(
            !rep.issues()
                .iter()
                .any(|i| i.rule == LintRule::StripCoverageIrregular
                    || i.rule == LintRule::StripOutsideFrame),
            "geometry issues: {:?}",
            rep.issues()
        );
        assert_eq!(rep.strips_walked(), 2);
    }

    #[test]
    fn coverage_gap_warns() {
        // Single strip covering only the top half of a 16-row frame.
        let mut f = frame_one_strip(0);
        f[6..8].copy_from_slice(&16u16.to_be_bytes()); // frame height 16
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::StripCoverageIrregular));
        assert!(rep.is_conformant()); // warning only
    }

    #[test]
    fn strip_outside_frame_is_error() {
        let mut f = frame_one_strip(0);
        f[18..20].copy_from_slice(&12u16.to_be_bytes()); // y_bottom 12 > height 8
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::StripOutsideFrame));
    }

    #[test]
    fn partial_width_strip_warns() {
        let mut f = frame_one_strip(0);
        f[20..22].copy_from_slice(&10u16.to_be_bytes()); // x_bottom 10 < width 16
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::StripNotFullWidth));
        assert!(has_rule(&rep, LintRule::StripWidthNotMultipleOf4));
    }

    #[test]
    fn empty_rect_is_error() {
        let mut f = frame_one_strip(0);
        f[14..16].copy_from_slice(&8u16.to_be_bytes()); // y_top 8
        f[18..20].copy_from_slice(&8u16.to_be_bytes()); // y_bottom 8 (empty)
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::StripEmptyRect));
    }

    #[test]
    fn mixed_strip_kinds_warn() {
        let strip_size = (STRIP_HEADER_SIZE as u16).to_be_bytes();
        let mut f = Vec::new();
        let frame_length = FRAME_HEADER_SIZE + 2 * STRIP_HEADER_SIZE;
        f.push(0x00);
        f.extend_from_slice(&[0, 0, frame_length as u8]);
        f.extend_from_slice(&16u16.to_be_bytes());
        f.extend_from_slice(&16u16.to_be_bytes());
        f.extend_from_slice(&2u16.to_be_bytes());
        for id in [0x1000u16, 0x1100] {
            f.extend_from_slice(&id.to_be_bytes());
            f.extend_from_slice(&strip_size);
            f.extend_from_slice(&0u16.to_be_bytes());
            f.extend_from_slice(&0u16.to_be_bytes());
            f.extend_from_slice(&8u16.to_be_bytes());
            f.extend_from_slice(&16u16.to_be_bytes());
        }
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::MixedStripKinds));
    }

    #[test]
    fn inheritance_flag_on_intra_frame_warns() {
        let mut f = frame_one_strip(0);
        f[0] = 0x01;
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::FlagsInheritanceOnIntraFrame));
        assert!(rep.is_conformant());
    }

    #[test]
    fn issue_display_carries_rule_location_and_spec_ref() {
        let mut f = frame_one_strip(0);
        f[10..12].copy_from_slice(&0x1200u16.to_be_bytes());
        let rep = lint_frame(&f);
        let issue = rep
            .issues()
            .iter()
            .find(|i| i.rule == LintRule::UnknownStripId)
            .unwrap();
        let s = issue.to_string();
        assert!(s.contains("error"), "{s}");
        assert!(s.contains("strip 0"), "{s}");
        assert!(s.contains("01-frame-and-strip.md"), "{s}");
    }

    // ---- chunk-layer fixtures (milestone 2) ----

    /// One chunk: 4-byte header (`id`, inclusive size) + body.
    fn chunk(id: u16, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(4 + body.len());
        v.extend_from_slice(&id.to_be_bytes());
        v.extend_from_slice(&((4 + body.len()) as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    /// Wrap `(strip_id, wire_y_top, wire_y_bottom, chunk_stream)`
    /// strips into a frame with correct length accounting.
    fn wrap_frame(width: u16, height: u16, strips: &[(u16, u16, u16, Vec<u8>)]) -> Vec<u8> {
        let strips_len: usize = strips
            .iter()
            .map(|(_, _, _, p)| STRIP_HEADER_SIZE + p.len())
            .sum();
        let frame_length = FRAME_HEADER_SIZE + strips_len;
        let mut v = Vec::with_capacity(frame_length);
        v.push(0x00);
        v.extend_from_slice(&[
            ((frame_length >> 16) & 0xff) as u8,
            ((frame_length >> 8) & 0xff) as u8,
            (frame_length & 0xff) as u8,
        ]);
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&(strips.len() as u16).to_be_bytes());
        for (id, y_top, y_bottom, payload) in strips {
            v.extend_from_slice(&id.to_be_bytes());
            v.extend_from_slice(&((STRIP_HEADER_SIZE + payload.len()) as u16).to_be_bytes());
            v.extend_from_slice(&y_top.to_be_bytes());
            v.extend_from_slice(&0u16.to_be_bytes());
            v.extend_from_slice(&y_bottom.to_be_bytes());
            v.extend_from_slice(&width.to_be_bytes());
            v.extend_from_slice(payload);
        }
        v
    }

    /// A conformant 16×8 single-strip intra chunk stream: full V4 +
    /// full V1 (one entry each) + `0x3200` vector chunk (8 MBs).
    fn conformant_intra_stream() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x2000, &[10, 20, 30, 40, 0, 0]));
        p.extend_from_slice(&chunk(0x2200, &[10, 20, 30, 40, 0, 0]));
        p.extend_from_slice(&chunk(0x3200, &[0u8; 8]));
        p
    }

    #[test]
    fn handcrafted_conformant_intra_frame_is_clean() {
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, conformant_intra_stream())]);
        let rep = lint_frame(&f);
        assert!(rep.is_clean(), "unexpected issues: {:?}", rep.issues());
    }

    #[test]
    fn selective_update_on_intra_strip_is_error() {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x2000, &[10, 20, 30, 40, 0, 0]));
        // Selective V1 update: flag word bit 31 ⇒ slot 0, one entry.
        let mut body = 0x8000_0000u32.to_be_bytes().to_vec();
        body.extend_from_slice(&[1, 2, 3, 4, 0, 0]);
        p.extend_from_slice(&chunk(0x2300, &body));
        p.extend_from_slice(&chunk(0x3200, &[0u8; 8]));
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p.clone())]);
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::SelectiveUpdateOnIntraStrip));
        // The same stream on an inter strip is fine.
        let f = wrap_frame(16, 8, &[(0x1100, 0, 8, p)]);
        let rep = lint_frame(&f);
        assert!(!has_rule(&rep, LintRule::SelectiveUpdateOnIntraStrip));
    }

    #[test]
    fn header_only_codebook_on_intra_strip_warns() {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x2000, &[]));
        p.extend_from_slice(&chunk(0x2200, &[10, 20, 30, 40, 0, 0]));
        p.extend_from_slice(&chunk(0x3200, &[0u8; 8]));
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p.clone())]);
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::HeaderOnlyCodebookOnIntraStrip));
        assert!(rep.is_conformant()); // warning only
                                      // Header-only on an inter strip is the documented reuse signal.
        let f = wrap_frame(16, 8, &[(0x1100, 0, 8, p)]);
        let rep = lint_frame(&f);
        assert!(!has_rule(&rep, LintRule::HeaderOnlyCodebookOnIntraStrip));
    }

    #[test]
    fn misaligned_codebook_payload_is_error() {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x2000, &[1, 2, 3, 4, 5])); // 5 % 6 != 0
        p.extend_from_slice(&chunk(0x3200, &[0u8; 8]));
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p)]);
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::CodebookPayloadMisaligned));
    }

    #[test]
    fn oversized_codebook_is_error() {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x2000, &vec![0u8; 257 * 6]));
        p.extend_from_slice(&chunk(0x3200, &[0u8; 8]));
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p)]);
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::CodebookTooManyEntries));
    }

    #[test]
    fn unknown_chunk_id_is_error_and_walk_continues() {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x3300, &[0u8; 2])); // outside taxonomy
        p.extend_from_slice(&chunk(0x2001, &[0u8; 6])); // low byte non-zero
        p.extend_from_slice(&conformant_intra_stream());
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p)]);
        let rep = lint_frame(&f);
        let unknown = rep
            .issues()
            .iter()
            .filter(|i| i.rule == LintRule::UnknownChunkId)
            .count();
        assert_eq!(unknown, 2, "issues: {:?}", rep.issues());
    }

    #[test]
    fn chunk_overrun_and_undersize_are_errors() {
        // Declared size overruns the strip payload.
        let mut over = chunk(0x2000, &[0u8; 6]);
        over[3] = 60; // inflate declared size
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, over)]);
        assert!(has_rule(&lint_frame(&f), LintRule::ChunkOverrunsStrip));

        // Declared size smaller than the chunk header.
        let mut under = chunk(0x2000, &[]);
        under[3] = 2;
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, under)]);
        assert!(has_rule(&lint_frame(&f), LintRule::ChunkSizeUnderHeader));
    }

    #[test]
    fn unclaimed_strip_bytes_are_reported() {
        let mut p = conformant_intra_stream();
        p.extend_from_slice(&[0xaa, 0xbb]); // 2 bytes no chunk claims
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p)]);
        assert!(has_rule(&lint_frame(&f), LintRule::ChunkHeaderTruncated));
    }

    #[test]
    fn v1_before_v4_order_warns() {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x2200, &[10, 20, 30, 40, 0, 0]));
        p.extend_from_slice(&chunk(0x2000, &[10, 20, 30, 40, 0, 0]));
        p.extend_from_slice(&chunk(0x3200, &[0u8; 8]));
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p)]);
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::CodebookOrderNotV4ThenV1));
        assert!(rep.is_conformant());
    }

    #[test]
    fn duplicate_flavour_codebook_chunk_warns() {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x2000, &[10, 20, 30, 40, 0, 0]));
        p.extend_from_slice(&chunk(0x2000, &[11, 21, 31, 41, 0, 0]));
        p.extend_from_slice(&chunk(0x2200, &[10, 20, 30, 40, 0, 0]));
        p.extend_from_slice(&chunk(0x3200, &[0u8; 8]));
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p)]);
        assert!(has_rule(&lint_frame(&f), LintRule::DuplicateCodebookChunk));
    }

    #[test]
    fn chunk_after_vector_chunk_is_error() {
        let mut p = conformant_intra_stream();
        p.extend_from_slice(&chunk(0x2200, &[10, 20, 30, 40, 0, 0]));
        let f = wrap_frame(16, 8, &[(0x1000, 0, 8, p)]);
        assert!(has_rule(&lint_frame(&f), LintRule::ChunkAfterVectorChunk));
    }

    #[test]
    fn mixed_pixel_modes_across_strips_is_error_reported_once() {
        let mut yuv = Vec::new();
        yuv.extend_from_slice(&chunk(0x2000, &[10, 20, 30, 40, 0, 0]));
        yuv.extend_from_slice(&chunk(0x2200, &[10, 20, 30, 40, 0, 0]));
        yuv.extend_from_slice(&chunk(0x3200, &[0u8; 4]));
        let mut gray = Vec::new();
        gray.extend_from_slice(&chunk(0x2400, &[10, 20, 30, 40]));
        gray.extend_from_slice(&chunk(0x2600, &[10, 20, 30, 40]));
        gray.extend_from_slice(&chunk(0x3200, &[0u8; 4]));
        let f = wrap_frame(16, 8, &[(0x1000, 0, 4, yuv), (0x1000, 0, 4, gray)]);
        let rep = lint_frame(&f);
        let mixed = rep
            .issues()
            .iter()
            .filter(|i| i.rule == LintRule::MixedPixelModes)
            .count();
        assert_eq!(mixed, 1, "issues: {:?}", rep.issues());
        assert_eq!(
            rep.issues()
                .iter()
                .find(|i| i.rule == LintRule::MixedPixelModes)
                .unwrap()
                .strip,
            Some(1)
        );
    }

    #[test]
    fn malformed_selective_update_payload_is_error() {
        let mut p = Vec::new();
        p.extend_from_slice(&chunk(0x2100, &[0x80, 0x00])); // truncated flag word
        p.extend_from_slice(&chunk(0x3100, &0u32.to_be_bytes()));
        let f = wrap_frame(16, 4, &[(0x1100, 0, 4, p)]);
        let rep = lint_frame(&f);
        assert!(has_rule(&rep, LintRule::SelectiveUpdatePayloadMalformed));
        assert!(!has_rule(&rep, LintRule::SelectiveUpdateOnIntraStrip));
    }

    #[test]
    fn stateful_encoder_intra_and_inter_frames_are_clean() {
        use crate::encoder::CinepakEncoder;
        let (w, h) = (32u32, 16u32);
        let mut enc = CinepakEncoder::new();
        let opts = EncoderOptions::default();
        let f0: Vec<u8> = (0..(w * h * 3)).map(|i| (i % 199) as u8).collect();
        let mut f1 = f0.clone();
        for px in f1.iter_mut().take(48) {
            *px = px.wrapping_add(90);
        }
        let intra = enc.encode_intra(&f0, w, h, opts).unwrap();
        let inter = enc.encode_inter(&f1, w, h, opts).unwrap();
        for (label, bytes) in [("intra", &intra), ("inter", &inter)] {
            let rep = lint_frame(bytes);
            assert!(rep.is_clean(), "{label} frame issues: {:?}", rep.issues());
        }
    }

    #[test]
    fn gray_encoder_output_is_clean() {
        use crate::encoder::encode_gray8;
        let (w, h) = (16u32, 16u32);
        let gray: Vec<u8> = (0..(w * h)).map(|i| (i * 7 % 256) as u8).collect();
        let bytes = encode_gray8(&gray, w, h, EncoderOptions::default()).unwrap();
        let rep = lint_frame(&bytes);
        assert!(rep.is_clean(), "issues: {:?}", rep.issues());
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // Cheap in-test sweep over adversarial shapes.
        let mut cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0xff; 9],
            vec![0xff; 10],
            vec![0x00; 64],
            vec![0xff; 64],
        ];
        let mut f = frame_one_strip(3);
        f[9] = 0xff; // absurd strip count
        cases.push(f);
        for c in cases {
            let _ = lint_frame(&c);
        }
    }
}
