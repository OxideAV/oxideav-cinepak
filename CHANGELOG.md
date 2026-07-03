# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Round 383 milestone 6 (conformance lint — encoder self-conformance
  property test + fuzz target): new integration test
  `tests/lint_encoder_conformance.rs` locking "every public encode
  entry point emits a stream the crate\'s own linter finds completely
  clean (zero errors *and* zero warnings)": the 8 stateless intra RGB
  entry points (baseline, best-strips, RD-grid ×2, 3-axis ×2,
  per-strip ×2), the 3 grayscale entry points, both stateless inter
  entry points (against a decoder-reconstructed previous frame), a
  6-frame multi-strip GOP through the auto-routing stateful encoder
  checked via `lint_sequence`, the stateful gray paths, the
  byte-budget rate-controlled paths, a `vintage_compat` GOP checked
  under the vintage lint profile (closing the encode/verify loop on
  skip-heavy inter frames with header-only reuse chunks), and a
  `from_quality` sweep (q ∈ {0, 25, 50, 75, 100}). An encoder
  regression that mis-accounts a chunk size, reorders codebook
  chunks, or emits an out-of-range vector index now trips `cargo
  test` directly. Plus a new `lint_frame` cargo-fuzz target (10th
  harness): drives arbitrary bytes through `lint_frame` and both
  option profiles, asserting total-function behaviour (a `LintReport`
  for every input, no panic/overflow/OOM), profile monotonicity
  (gated profiles only ever add findings) plus `strips_walked`
  agreement, and that every issue renders through `Display`. 46-second
  local smoke run: 796 672 executions, zero crashes.
- Round 383 milestone 5 (conformance lint — vintage profile +
  sequence context + `lint_sequence`): `LintOptions` gains two knobs
  (with `with_vintage` / `with_sequence_start` builders).
  `vintage: bool` enforces the vintage-player structural constraints
  the encoder\'s `EncoderOptions::vintage_compat` targets, closing the
  encode/verify loop: `VintageStripCountExceeded` (spec 01 §2.3,
  > 3 strips) and `VintageCodebookPairViolation` (spec 01 §2.3 / 02
  §2.2 — both V4 and V1 codebook chunks present per strip, header-only
  allowed, strict V4-then-V1 order). `sequence_start: bool` flags
  previous-frame/previous-codebook dependence on a stream\'s first
  frame or a seek target: `PrevFrameDependencyAtSequenceStart` fires
  on a `0x3100` chunk coding at least one skip macroblock (spec 03 §6:
  reusing an undefined previous reconstruction is undefined output —
  a skip-free `0x3100` stays legal) and on any selective-update
  codebook chunk (spec 02 §4.1: nothing to merge into). New
  `lint_sequence(frames, opts)` maps a decode sequence to per-frame
  reports with `sequence_start` forced on the first frame. 7 new lib
  tests: the 3-strip ceiling in both directions, missing-V1 (T7
  shape) and V1-before-V4 pairing violations plus the header-only
  pair passing, `vintage_compat` stateful-encoder intra + inter
  output passing the vintage profile end-to-end, skip-at-start vs
  skip-free-`0x3100`-at-start, selective-at-start, sequence mapping
  flagging only the first frame, and a 4-frame GOP
  (`with_keyframe_interval(2)`) from the auto-routing encoder
  linting clean per-frame.
- Round 383 milestone 4 (conformance lint — intra codebook occupancy):
  new `VectorIndexOutOfRange` rule implementing spec 02 §3.2 ("the
  strip's vector chunk MUST NOT reference an index ≥ N; if it does,
  the decode of that vector is undefined"). On intra strips the linter
  accumulates per-flavour occupancy from the strip's own
  full-replacement chunks (entries `0..N−1` defined; duplicates take
  the max) and checks every `0x3200` V1 index and every `0x3000`
  V1/V4 index against it, reporting the first offending macroblock
  with its flavour, index, and the defined count. The check is
  deliberately conservative: it is suppressed when occupancy is
  unknowable — inter strips (inheritance per spec 02 §5.2 crosses the
  single-frame view), intra strips carrying a selective-update chunk
  (already an §2.3 error), or damaged codebook chunks (truncation /
  misalignment / oversize, each already reported) — and it runs
  deferred to the strip end so a codebook chunk appearing after the
  vector chunk (already a `ChunkAfterVectorChunk` error) still
  contributes its entries instead of cascading a false index-range
  finding. `0x3100` on an intra strip is excluded (already flagged;
  its updates are inter-style). 6 new lib tests: V1 out-of-range +
  in-range twin on `0x3200`, V4 sub-block out-of-range + in-range twin
  inside a mixed `0x3000` walk, inter-strip non-checking,
  selective-on-intra suppression, out-of-order codebook-after-vector
  still counting, and header-only-defines-nothing ⇒ any reference
  flagged.
- Round 383 milestone 3 (conformance lint — vector layer): the linter
  now derives each strip's macroblock count from its resolved geometry
  (spec 03 §1, `(height/4) × (width/4)`; skipped when the geometry is
  itself non-conformant so one root cause doesn't cascade) and checks:
  every strip carries exactly one vector chunk (spec 03 §2 ⇒
  `MissingVectorChunk`, reported only when the chunk walk reached the
  strip end without structural derailment), `0x3100` on an intra strip
  (§2 taxonomy: inter-only; skip codes need a previous-frame
  reconstruction per §8 ⇒ `InterVectorChunkOnIntraStrip`), `0x3200`
  payload length = MB count (§3.1), `0x3000` / `0x3100` group-walk
  byte balance via the r250/r253 typed walkers — truncation mid-walk ⇒
  `VectorPayloadLengthMismatch`, unconsumed payload bytes after the
  last macroblock ⇒ `VectorPayloadTrailingBytes` (§7 step 5: "any
  mismatch is a malformed stream"). 8 new lib tests: both length
  mismatch directions on `0x3200`, truncated + trailing `0x3000`
  walks, the spec §3.3 fixture-`Y8`-shaped inter walk balancing
  exactly (and failing on one extra byte), `0x3100`-on-intra both
  directions, missing-vector-chunk on codebook-only and empty strips,
  and the geometry-invalid skip gate. The milestone-1 synthetic
  fixtures were upgraded to carry conformant chunk streams now that
  empty strip payloads are (correctly) findings themselves.
- Round 383 milestone 2 (conformance lint — chunk layer): the linter
  now walks each strip's chunk stream per spec 02 §1 and reports:
  chunk-header truncation / `chunk_size < 4` / strip-payload overrun
  (§1, which together enforce the `Σ chunk_size == strip_size − 12`
  accounting), unknown chunk ids (§2.1 low-byte-zero taxonomy + spec
  03 §2.1 vector codes), selective-update chunks on intra strips (§2.3
  ⇒ Error), header-only full-replacement chunks on intra strips (§3.4
  vacuous ⇒ Warning), full-replacement payload arithmetic (entry-size
  alignment and the 256-entry ceiling, §3.1), selective-update payload
  structure via the r256 `CodebookEntries` walker (truncated flag
  word / entry, > 8 groups / 256 slots, §4.2–§4.3), mixed 12-bit-YUV +
  grayscale chunk families within one frame (spec 04 §4 MUST NOT ⇒
  Error, reported once with the offending strip/chunk), V1-before-V4
  codebook ordering (§2.2 ⇒ Warning), duplicate same-flavour codebook
  chunks (§2.2 ⇒ Warning), and any chunk following the strip's vector
  chunk (spec 03 §2 "exactly one vector chunk after its codebook
  chunks" ⇒ Error). `LintIssue.chunk` now carries the 0-based chunk
  index. 14 new lib tests: a handcrafted fully-conformant intra frame
  (clean), each rule in both fire and no-fire directions, the
  once-per-frame mixed-mode cap, and cleanliness of the stateful
  encoder's intra + inter output and the grayscale encoder's output.
- Round 383 (wire-format conformance lint — frame + strip layers): new
  `lint` module with `lint_frame` / `lint_frame_with` entry points and
  the `LintReport` / `LintIssue` / `LintRule` / `LintSeverity` /
  `LintOptions` types. Where the decoder answers "can these bytes be
  turned into pixels?", the linter answers the stricter structured
  question "do these bytes conform to the documented wire format, and
  if not, which rule, where, and how badly?" — every finding carries
  the violated rule, an `Error` (normative violation) vs `Warning`
  (SHOULD / encoder-convention / corpus-observation deviation)
  severity, the strip location, the byte offset, and the spec section
  the rule is grounded in (`LintRule::spec_ref`). This first milestone
  covers the frame and strip layers of
  `docs/video/cinepak/spec/01-frame-and-strip.md`: header truncation,
  `frame_length` under-header / buffer-overrun / `10 + Σ strip_size`
  accounting (§1.2/§3), zero strip count, zero and non-multiple-of-4
  frame dimensions (§1 + spec 03 §1), reserved `flags` bits (§1.1
  SHOULD ⇒ Warning), the `flags`-bit-0-on-all-intra-frame
  contradiction (§1.1 + spec 02 §5.2 ⇒ Warning), strip-header
  truncation / `strip_size < 12` / frame overrun (§2), the
  `{0x1000, 0x1100}` strip-id taxonomy (§2.1), mixed intra+inter strip
  kinds (§2.1 observation ⇒ Warning), the §2.2 y-sentinel geometry
  (empty rects, non-multiple-of-4 strip extents, strips outside the
  coded frame, non-full-width strips ⇒ Warning, non-contiguous
  vertical tiling ⇒ Warning). The walk is best-effort (reports as many
  independent issues as possible, never panics on arbitrary input) and
  read-only. 23 new lib tests cover every rule in both directions plus
  encoder-output cleanliness (single- and multi-strip) and an
  adversarial no-panic sweep. Later milestones extend the same report
  down the chunk, vector, and codebook-occupancy layers.
- New `encode_roundtrip` `cargo-fuzz` target. The existing harnesses all
  drive the decode / parse side; this one fuzzes the **encoder**: it
  builds attacker-shaped pixel buffers, dimensions (capped at 64×64), and
  option knobs (`from_quality` and a hand-built `EncoderOptions` flavour
  stressing `strip_count` / `skip_threshold`), encodes via `encode_rgb24`
  / `encode_gray8` / `encode_rgb24_inter`, and feeds every output back
  through `CinepakDecoder` to assert the encoder always emits a
  decoder-acceptable stream. This exercises the full `0x3000` / `0x3100`
  / `0x3200` vector-code dispatch — including the skip-free inter routing
  added in this round — across both pixel modes and the intra / inter
  paths. A 491,520-iteration local replay of the harness body over a
  synthetic input grid completed with no panic.

### Changed

- Skip-free inter strips now pick the cheapest legal vector code instead
  of always emitting `0x3100`. When an inter strip contains **no skip
  macroblocks** — every macroblock is recoded fresh, e.g. a scene cut
  where no previous-frame content can be reused — the `0x3100` form is
  wasteful: its skip / V1 / V4 VLC spends 2 bits per coded macroblock, so
  a 32-bit flag word holds only ~16 macroblocks and the chunk pays
  `ceil(N / 16) × 4` flag-word bytes. The same skip-free macroblock set
  is expressed identically by `0x3000` (mixed, 1 bit/MB ⇒ 32 MBs/group,
  `ceil(N / 32) × 4` flag-word bytes) and, when every coded macroblock is
  V1, by the flag-word-free `0x3200`. The encoder now routes a skip-free
  strip to `0x3200` (all-V1) or `0x3000` (any V4), keeping `0x3100` only
  when at least one macroblock skips. The index data is byte-identical
  across the three forms, so the reconstruction is unchanged; only the
  flag-word overhead shrinks. This mirrors the intra-strip dispatch and
  the reference encoder strategy in
  `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` §8 ("the
  encoder may also opt for `0x3000` on an inter strip when the
  previous-frame content cannot be reused"; the decoder accepts all three
  vector codes on either strip kind). Three new unit tests
  (`skipfree_all_v1_inter_strip_uses_v1_only_chunk`,
  `skipfree_mixed_inter_strip_uses_mixed_chunk`,
  `inter_strip_with_skips_keeps_inter_chunk`) lock the dispatch in all
  three directions with roundtrip checks.
- Round 371 (encoder V1-only intra vector chunk): the intra strip
  encoder now emits the `0x3200` V1-only vector chunk instead of the
  `0x3000` mixed form whenever every macroblock of the strip is
  V1-coded (flat / low-detail content). The `0x3200` form carries one
  index byte per macroblock with **no per-group flag word**, whereas
  the mixed `0x3000` form spends an extra `ceil(N / 32) × 4` bytes on
  all-zero flag words; for a single-strip 320×240 frame (4800
  macroblocks) that saves 600 bytes per all-V1 strip. This mirrors the
  reference encoder strategy documented in
  `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` §8 ("the
  encoder picks `0x3200` when every macroblock fits a V1 vector").
  Reconstruction is byte-identical to the prior `0x3000` output — the
  decoder expands the same V1 indices. Detailed strips that need at
  least one V4 macroblock continue to use `0x3000`. Two new unit tests
  (`flat_intra_strip_emits_v1_only_vector_chunk`,
  `detailed_intra_strip_keeps_mixed_vector_chunk`) lock the dispatch.

### Added

- Round 335 (resolved-RGB mixed-intra macroblock walker): new
  `vector::MixedIntraRgbBlocks<'a>` iterator plus the `MixedIntraRgbBlock`
  record and `MixedIntraCoding` enum. It is the resolved-RGB capstone
  over the r250 index-only `MixedIntraMacroblocks` walker: given a
  `0x3000` payload, the strip's resolved V1/V4 codebooks
  (`&[CodebookEntry]`), the macroblock-grid width, and the strip
  macroblock count, it yields each macroblock's fully reconstructed 4×4
  RGB block together with its `(mb_col, mb_row)` grid position and
  top-left pixel origin (spec §1.1 scan-order mapping `(i / C, i % C)`
  in `docs/video/cinepak/spec/03-vectors-and-macroblocks.md`). It
  composes the §3.2 V1/V4 classification with the §§4/5 macroblock
  expansion and the §3 YUV→RGB matrix (via `expand_v1_mb_rgb` /
  `expand_v4_mb_rgb`), giving validators and introspection tools
  pixel-exact reconstructed blocks without allocating a strided output
  raster — the framebuffer-free counterpart of the decoder's intra
  strip-reconstruction loop. An out-of-range codebook index, a
  truncated payload, or a zero grid width is reported as `Some(Err(_))`
  (or a construction error) and the iterator self-fuses. Nine unit
  tests cover all-V1 / all-V4 / mixed grids, the grayscale identity,
  position mapping, both index-out-of-range paths, truncation fusing,
  and byte-exact agreement with manual `MixedIntraMacroblocks` +
  codebook-lookup + expansion composition.
- Round 309 (typed §4/§5 + §3 macroblock RGB-pixel-block surface):
  two new free functions at `src/yuv.rs` — `expand_v1_mb_rgb(&CodebookEntry)
  -> [[[u8; 3]; 4]; 4]` and `expand_v4_mb_rgb(&[CodebookEntry; 4]) ->
  [[[u8; 3]; 4]; 4]` — that close the colour-conversion capstone above
  the r307 luminance/chroma plane-geometry surface. r307 stopped at
  plane geometry (`expand_v1_luma` / `expand_v4_luma` / `expand_v4_chroma`)
  deliberately *before* the YUV→RGB matrix; r309 composes the spec §4
  (V1 luminance-quadrant layout) / §5 (V4 sub-block layout +
  per-sub-block chroma) macroblock expansion of
  `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` with the
  spec §3 inverse matrix of
  `docs/video/cinepak/spec/04-yuv-rgb-matrix.md` (truncation-toward-zero
  on `U/2`, per-channel clamp to `[0, 255]`) to yield the concrete 4×4
  RGB pixel block of a single macroblock. V1 applies the entry's single
  `(U, V)` across the whole 4×4 (chroma 4:2:0 at 4×4 granularity, §4);
  V4 routes each entry's own `(U, V)` to its 2×2 sub-block (four chroma
  samples per macroblock, §5). For 8-bit grayscale entries (chroma
  zero) every pixel's three channels equal its luminance value (the §4
  grayscale identity). The functions are allocation-free and framework-
  feature-independent (`default-features = false` build picks them up):
  the standalone counterpart to the decoder's internal strided
  `draw_v1_mb_rgb` / `draw_v4_mb_rgb` writes, for validators,
  introspection tools, and re-encoders that want the resolved RGB
  geometry of one macroblock without an output framebuffer. Also
  re-exports the already-`pub` `yuv::yuv_to_rgb` from the crate root
  (previously reachable only as `oxideav_cinepak::yuv::yuv_to_rgb`).
  7 new lib tests (198 → 205) anchor each helper to the spec's own
  worked examples — the §4 `Y15` quadrant pattern under the verified
  `M10` chroma and the `M1` red primary, the §4.1 grayscale identity,
  the §5 `Y16` ramp routed through the four verified primary chroma
  pairs M1/M2/M3/M7 with per-sub-block placement, the V1-vs-V4
  layout-distinction case (uniform V4 quad ≠ V1 tiling, agree only at
  shared corners), the V4 grayscale identity, and a byte-for-byte
  cross-check against the decoder's own strided V1 write. New crate-root
  exports: `expand_v1_mb_rgb`, `expand_v4_mb_rgb`, `yuv_to_rgb`. Pure
  additive change — the decoder's RGB / grayscale pixel-write paths are
  untouched. Wall: `docs/video/cinepak/spec/03-vectors-and-macroblocks.md`
  (§4 V1 expansion + §5 V4 sub-block / chroma layout),
  `docs/video/cinepak/spec/04-yuv-rgb-matrix.md` (§3 inverse matrix +
  §3.1/§3.2 pinned conventions + §4.1 grayscale identity), own crate
  `src/yuv.rs` + `src/codebook.rs` (`CodebookEntry`) + `src/decoder.rs`
  (cross-check reference). No external library source, no web search, no
  GH issues opened.
- Round 289 (depth-mode profiling — per-training-phase cost
  decomposition): new `phases` mode in the `examples/profile_cinepak.rs`
  driver that decomposes one stateless intra encode into the marginal
  wall-time of each codebook-training phase. It starts from a
  training-minimal `EncoderOptions` base (median-cut only:
  `kmeans_pp_init = false`, `lloyd_max_iter = 0`, `lbg_max_passes = 0`,
  `pcl_max_iter = 0`) and toggles each phase ON cumulatively —
  `+ lloyd_max_iter=2`, `+ lbg_max_passes=8`, `+ pcl_max_iter=2`,
  `+ kmeans_pp_init (4 lloyd)` — measuring the median of 3 rep groups
  per row and reporting the marginal ms each phase adds plus its
  wire-size effect. The mode drives only the public `encode_rgb24`
  entry point through public `EncoderOptions` fields, so no encoder
  code is instrumented and the measured wire output is exactly what
  those option vectors already produce. Finding (Apple M4 Max,
  release, captured in `profile/README.md`): **LBG split-refinement
  (`lbg_max_passes`) owns the encoder hot path** — +15.4 ms on
  320×240 (5.4× the median-cut base, ~62 % of full-options encode
  time) and +64.9 ms on 640×480, for a <5 % wire effect — because
  each of its 8 default passes re-runs a full Lloyd assignment over
  the O(vectors × K) `nearest` scan; k-means++ is the second cost
  centre (+4.7 to +21.8 ms), while Lloyd refinement and
  post-classification polish are near-free. Pure profiling artefact:
  no `src/` change, no encode-behaviour change, no Cargo.toml change.
  The new mode is wired into the `all` aggregate, the usage/help
  strings, and the module doc-comment. Wall: own crate
  `src/encoder.rs` public surface (`EncoderOptions` fields,
  `encode_rgb24`) read only to drive the harness; no external library
  source, no web search, no GH issues opened.
- Round 270 (standalone STAB chunk-header parser): new
  `StabHeader::parse_chunk(chunk: &[u8]) -> Result<(StabHeader,
  `StabHeader::parse_chunk(chunk: &[u8]) -> Result<(StabHeader,
  &[u8])>` associated function at `src/film.rs` plus the
  `STAB_HEADER_SIZE = 16` constant. The function parses the STAB
  chunk's fixed 16-byte header (`'STAB'` signature + chunk length +
  `base_frequency` + `num_entries`) per
  `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 84-91 and
  returns the parsed `StabHeader` paired with the records-only byte
  slice that follows — bounded to exactly `num_entries *
  SAMPLE_RECORD_SIZE` bytes, i.e. exactly the slice the round-261
  `Samples::new` walker expects. This closes the bridge gap left by
  r261: that iterator required the caller to compute the records-slice
  offset (`FILM_HEADER_MIN_SIZE + fdsc_len + 16`) by hand and trim it
  themselves; `parse_chunk` does both from a STAB chunk slice in one
  read-only step, so a partial-header streamer / validator / sister-
  format demuxer can do `let (hdr, recs) =
  StabHeader::parse_chunk(chunk)?; let it = Samples::new(recs)?;`
  without the `FilmDemuxer::parse` round-trip and its
  `Vec<SampleRecord>` allocation. The wire `length` field (bytes 4-7)
  is read but **not** used for offset arithmetic — `Sega_FILM.wiki`
  line 92 records that some titles (Burning Rangers, version `'1.09'`)
  omit the first 16 bytes from it — so correctness rests on
  `num_entries` alone; trailing bytes beyond the declared table are
  excluded from the returned slice. Errors: chunk shorter than
  `STAB_HEADER_SIZE`, bad `'STAB'` signature, or a `num_entries * 16`
  record table that overruns the chunk (with `checked_mul` /
  `checked_add` guards against arithmetic overflow on a hostile
  `num_entries`). 10 new lib tests (182 → 192) cover: the
  `STAB_HEADER_SIZE` constant, empty-table parse + feed into
  `Samples`, single-record parse + classified-yield, the C3-style
  5-sample table agreeing with the round-261 classification, the
  short-length-field (Burning-Rangers) tolerance, trailing-bytes
  exclusion, truncated-header / bad-signature / truncated-records
  rejection, and an end-to-end agreement check that carves the STAB
  chunk out of `build_minimal_film()` and asserts `parse_chunk` ⇒
  `(StabHeader, records)` matches `FilmDemuxer::parse`'s `stab_header`
  + per-record stream. New public export: `STAB_HEADER_SIZE`. Pure
  additive change — no decoder / encoder / `FilmDemuxer::parse`
  behaviour change, no Cargo.toml changes. Wall:
  `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` (lines 84-92 —
  STAB chunk header layout + the length-field-omits-16-bytes note),
  already-shipped public surface (`StabHeader`, `Samples`,
  `SAMPLE_RECORD_SIZE`, `STAB_SIGNATURE`, `FilmDemuxer::parse`,
  `build_minimal_film` test helper). No external library source, no
  web search, no GH issues opened.
- Round 261 (typed STAB sample-records walker): new
  `film::Samples` iterator at `src/film.rs` that walks a STAB
  sample-records byte slice and yields one
  `film::SampleRecordEntry { index, record, kind }` per 16-byte
  row in wire order. Wire-format reference: `Sega_FILM.wiki` lines
  84-116 (`docs/video/cinepak/reference/wiki/Sega_FILM.wiki`).
  `Samples::new(records)` takes the `num_entries * 16` bytes that
  follow the 16-byte STAB chunk header — equivalently the suffix
  of the FILM header buffer starting at offset
  `FILM_HEADER_MIN_SIZE + fdsc_len + 16` — and rejects misaligned
  lengths (must be a multiple of `SAMPLE_RECORD_SIZE = 16`) up
  front. The iterator is read-only, content-agnostic, and
  `ExactSizeIterator + FusedIterator` — every 16-byte record
  decodes to a valid `SampleRecord` + `SampleKind` by construction
  so no per-yield error surface is needed. Companion enum
  `film::SampleKind` classifies each record into
  `Audio { well_formed }` / `VideoKeyframe { timestamp_ticks,
  next_frame_ticks }` / `VideoInter { timestamp_ticks,
  next_frame_ticks }` per `sample_info_1 == 0xFFFFFFFF` (audio
  sentinel, line 102), top-bit clear ⇒ keyframe (line 104), low
  31 bits = timestamp (line 116), `sample_info_2` = next-frame
  ticks for video (line 116) / well-formedness constant `1` for
  audio (line 116). `SampleKind::from_record(rec)` is the
  standalone classification surface the iterator re-uses; the
  enum also exposes `is_audio()` / `is_keyframe()` / `is_video()`
  predicate accessors. Accessors `remaining()`, `cursor()`,
  `next_index()`, `records_bytes()` round out the typed surface.
  The container-side analogue to r240 / r243 / r246 / r250 /
  r253 / r256: with this iterator the read-only typed surface now
  covers every Cinepak wire layer from FILM container-table
  (`Samples`) → strip (`FrameStrips`) → chunk (`StripChunks`) →
  per-MB / per-slot (`V1OnlyMacroblocks` / `MixedIntraMacroblocks`
  / `InterMacroblocks` / `CodebookEntries`). Callers who already
  hold the STAB records slice — partial-header streamers, custom
  demuxers that share STAB parsing with sister formats, container
  validators that want per-record granularity without the
  `FilmDemuxer::parse` round-trip — can drive a typed per-sample
  loop with one dependency. The existing
  `FilmDemuxer::video_samples` / `audio_samples` / `keyframes`
  helpers stay as in-place predicate-filter shortcuts; `Samples`
  is the per-record typed primitive that sits behind them.
  New public exports: `Samples`, `SampleKind`,
  `SampleRecordEntry`, `SAMPLE_RECORD_SIZE`. 17 new lib tests
  (165 → 182). Wall: `docs/video/cinepak/reference/wiki/Sega_FILM.wiki`
  (lines 84-116 — STAB record layout + `sample_info_1` keyframe-bit
  + timestamp semantics + audio sentinel + `sample_info_2`
  well-formedness), already-shipped public surface (`SampleRecord`,
  `SampleRecord::is_audio`, `SampleRecord::is_keyframe`,
  `SampleRecord::is_well_formed_audio`, `SampleRecord::timestamp_ticks`,
  `SampleRecord::next_frame_ticks`, `FilmDemuxer::parse`,
  `build_minimal_film` test helper). No external library source,
  no web search, no GH issues opened.
- Round 256 (typed codebook-chunk-payload entry walker): new
  `codebook::CodebookEntries` iterator at `src/codebook.rs`
  implementing spec §3 (full-replacement chunks `0x2000` /
  `0x2200` / `0x2400` / `0x2600`) and §4 (selective-update chunks
  `0x2100` / `0x2300` / `0x2500` / `0x2700`) of
  `docs/video/cinepak/spec/02-codebooks.md` — the per-slot wire
  structure of a codebook chunk's `chunk_size - 4` payload bytes.
  `CodebookEntries::new(kind, payload)` takes a chunk-payload
  byte slice and the classified `CodebookChunkKind` (typically
  the `StripChunkKind::Codebook(_)` value yielded by the
  round-243 `StripChunks` iterator) and yields one
  `Result<CodebookEntryRecord>` per occupied slot, in wire order.
  Both chunk styles flow through the same `Iterator::Item` shape
  (`CodebookEntryRecord { slot: u16, entry: CodebookEntry }`) so
  callers can drive a generic codebook accumulator with one loop
  regardless of full-vs-selective: full-replacement walks yield
  slots `0`, `1`, …, `N-1` in payload order; selective-update
  walks yield slots `group_base + bit_offset` where `group_base
  ∈ {0, 32, 64, …, 224}` is the group's first slot and
  `bit_offset ∈ 0..=31` is the per-bit position with bit 31
  (MSB-first scan per spec §4.2) ⇒ slot 0 of the group, bit 0 ⇒
  slot 31 of the group. The walker is read-only and
  content-agnostic — `apply_codebook_chunk` /
  `apply_codebook_chunk_with` stay in the in-place decoder path.
  Callers wanting per-slot wire boundaries (validators, fuzz
  harnesses that need per-entry granularity, wire-format
  introspection tools) can take this single dependency without
  pulling in the full `Codebook` apply path. Up-front validation:
  full-replacement payloads must be a multiple of
  `kind.mode.entry_size()` and ≤ 256 entries (rejected at
  construction); selective-update payloads are validated lazily
  per-yield. The Sega Saturn deviant 0x5FC-byte trailing-pad
  variant per `Sega_FILM.wiki` line 143 stays under
  `apply_codebook_chunk_with(..., tolerate_trailing: true)` — the
  typed walker is strict. Header-only chunks (spec §3.4,
  `chunk_size = 0x0004`) yield zero entries (empty iterator
  correctly models the "no update / reuse previous codebook"
  signal). Truncation (mid-flag-word / mid-entry) and >256-slot
  overrun on selective-update payloads are reported per-yield as
  `Some(Err(_))` and the iterator fuses to `None` afterwards.
  `mode()`, `style()`, `cursor()`, `remaining_bytes()`,
  `payload()` accessors round out the typed surface. The §3-§4
  capstone for the per-strip-chunk typed surface: r240
  (`FrameStrips`, frame → strips) + r243 (`StripChunks`, strip
  → chunks) + r246 (`V1OnlyMacroblocks`, `0x3200` MB-stream) +
  r250 (`MixedIntraMacroblocks`, `0x3000` MB-stream) + r253
  (`InterMacroblocks`, `0x3100` MB-stream) + r256
  (`CodebookEntries`, codebook-chunk payload) — every layer of
  the wire format from frame down to per-slot / per-macroblock
  now has a read-only typed iterator that composes back up to
  `StripChunks` via `StripChunkEntry::payload`. Tests at
  `src/codebook.rs::tests` (18 added) exercise: spec §3.1 fixture
  `T1a` (single-entry 0x2200 ⇒ slot 0 carries
  `(72, 72, 72, 72, -37, +90)`), sequential-slot full-replacement
  happy path (3-slot 0x2000), grayscale full-replacement
  (`0x2400` 4-byte entries, `u`/`v` zeroed), header-only
  chunk-yields-none for both styles, full-replacement
  misaligned-payload + >256-entry construction-time rejection,
  selective-update single-bit `bit 31 ⇒ slot 0`, MSB-first scan
  direction (`bit 0 ⇒ slot 31`), multi-group walk (slot 33 in
  group 1 + slot 255 in group 7 with intermediate all-zero
  groups), grayscale selective-update (`0x2500`),
  truncated-mid-flag-word fuse, truncated-mid-entry fuse,
  9th-group overrun-past-256-slots fuse, cross-check vs
  `apply_codebook_chunk` on `encode_selective_chunk` output
  (slots 0 + 33 + 200), cross-check vs `apply_codebook_chunk` on
  `encode_full_chunk` output (16 entries), accessor advance
  contract for full-replacement, selective cursor advance past
  flag-word + entry, and end-to-end composition with
  `StripChunks` (a header-only 0x2000 + single-entry 0x2200
  strip-payload pair driven through both iterators without ever
  touching `apply_codebook_chunk`). New public exports from the
  crate root: `CodebookEntries`, `CodebookEntryRecord`.

- Round 253 (typed `0x3100` per-macroblock walker): new
  `vector::InterMacroblocks` iterator at `src/vector.rs`
  implementing spec §3.3 of
  `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` — the
  inter-with-skip vector chunk encodes each macroblock as a 1- or
  2-bit variable-length code packed MSB-first into the flag-word
  stream (`0` ⇒ SKIP, `10` ⇒ V1, `11` ⇒ V4); codes can straddle a
  flag-word boundary, in which case the V1 / V4 selector bit is
  read from the **next** flag word's MSB and the deferred
  macroblock's index bytes belong to that next group's index data
  block (spec §3.3 step 3 + step 5, the `pending_set` rule).
  `InterMacroblocks::new(payload, mb_count)` returns a read-only
  iterator that walks one group at a time — loading the flag
  word, classifying up to 33 macroblocks (one resolved-pending
  entry + up to 32 fresh codes) into an internal scan-order
  buffer, reading the group's index data from the payload, then
  yielding one `InterEntry { index, kind }` per macroblock where
  `kind` is `InterMb::Skip` / `InterMb::V1(u8)` /
  `InterMb::V4([u8; 4])` matching the spec §3.3 grammar. The
  §3.3 sibling of round-246's `V1OnlyMacroblocks` (§3.1) and
  round-250's `MixedIntraMacroblocks` (§3.2): a
  `StripChunkEntry::payload` from round-243's `StripChunks`
  whose `kind` resolves to `VectorChunkKind::InterWithSkip` feeds
  straight into `InterMacroblocks::new`, completing the per-MB
  typed surface across all three vector-chunk codes (`0x3200` /
  `0x3000` / `0x3100`). The walker is read-only and
  content-agnostic — codebook expansion (spec §§4–5) and SKIP
  semantics (spec §6, "reuse the previous frame's reconstructed
  block") stay in `decoder::decode_strip_chunks`'s hot path.
  `cursor()`, `remaining()`, `mb_count()`, and `payload()`
  accessors round out the typed surface; `Iterator::size_hint`
  reports the exact remaining count based on `mb_count`. Like
  the round-250 mixed-intra walker, per-group byte sizes depend
  on the in-group SKIP / V1 / V4 mix and the `pending_set` state
  so length-consistency can only be checked during the walk:
  truncation (mid-flag-word / mid-V1-index / mid-V4-index) is
  reported per-yield as `Some(Err(_))` and the iterator fuses to
  `None` afterwards. A dangling `pending_set` after the final
  macroblock yields a per-yield error as well. Tests at
  `src/vector.rs::tests` (12 added) exercise: spec §3.3 fixture
  `Y8` (16-MB strip, 1 V1 update + 15 SKIPs, flag word
  `0x80000000`, 5-byte payload), spec §3.3 fixture `Y10` (32-MB
  strip, 2-flag-word layout exercising the group-refill path on
  the 32-MB boundary), an all-SKIP 32-MB strip (single 4-byte
  flag word, no index data), a single V4 update + 14 SKIPs (rich
  V4-vs-SKIP mix), empty-strip (`mb_count == 0`), the three
  truncation-fuse paths, `size_hint` exactness, the `cursor`
  per-yield advance contract, a cross-check against
  `decode_vector_chunk(0x3100, …)` for a 64-MB mixed-mix payload
  that forces multiple selector-spillover boundaries, and an
  explicit `pending_set` straddle stress (31 SKIPs + V1 + SKIP)
  that crosses the 32-bit flag-word boundary inside the V1's
  `10` code. New public exports from the crate root: `InterEntry`,
  `InterMacroblocks`, `InterMb`.

- Round 250 (typed `0x3000` per-macroblock walker): new
  `vector::MixedIntraMacroblocks` iterator at `src/vector.rs`
  implementing spec §3.2 of
  `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` — the
  mixed-intra vector chunk is a sequence of one-or-more groups,
  each starting with a 4-byte big-endian flag word whose 32 bits
  (scanned MSB-first) classify per-macroblock coding mode as V1
  (bit clear ⇒ 1 index byte) or V4 (bit set ⇒ 4 index bytes); a
  group covers exactly 32 macroblocks unless the strip's
  macroblock count is exhausted before 32.
  `MixedIntraMacroblocks::new(payload, mb_count)` returns a
  read-only iterator that classifies each group's flag word into
  an inline 32-entry buffer and reads the per-MB index bytes from
  the payload, yielding one `MixedIntraEntry { index, kind }` per
  macroblock where `kind` is `MixedIntraMb::V1(u8)` or
  `MixedIntraMb::V4([u8; 4])` matching the spec §3.2 selector
  semantics. The §3.2 mirror of round-246's `V1OnlyMacroblocks`:
  a `StripChunkEntry::payload` from round-243's `StripChunks`
  whose `kind` resolves to `VectorChunkKind::IntraMixed` feeds
  straight into `MixedIntraMacroblocks::new`, completing the
  per-MB typed surface for the two intra vector-chunk codes
  (`0x3200` / `0x3000`). The walker is read-only and
  content-agnostic — codebook expansion (spec §§4–5) and pixel
  writes stay in `decoder::decode_strip_chunks`'s hot path.
  `cursor()`, `remaining()`, `mb_count()`, and `payload()`
  accessors round out the typed surface; `Iterator::size_hint`
  reports the exact remaining count based on `mb_count`. Unlike
  the round-246 V1-only walker, per-group byte sizes depend on
  the in-group V1/V4 mix so length-consistency can only be
  checked during the walk: truncation (mid-flag-word /
  mid-V1-index / mid-V4-index) is reported per-yield as
  `Some(Err(_))` and the iterator fuses to `None` afterwards.
  Tests at `src/vector.rs::tests` (12 added) exercise: spec §3.2
  fixture `Y9` (all-V4 16-MB strip, flag word `0xffff0000`,
  4-byte V4 quadruples in scan order), spec §3.2 fixture `Y12`
  (checkerboard V1/V4, flag word `0x5a5a0000`), spec §3.2
  fixture `Y14` (two `0xffffffff` flag-word groups across 64-MB
  strip, exercising the group-refill path), an all-V1 happy
  path (flag word `0x00000000`), empty-strip
  (`mb_count == 0`), the three truncation-fuse paths,
  `size_hint` exactness, the `cursor`/`remaining` per-yield
  advance contract, a cross-check against
  `decode_vector_chunk(0x3000, …)` for the `Y12` fixture, and
  a group-boundary stress at exactly 32 MBs (33-MB strip
  forces two flag words). New public exports from the crate
  root: `MixedIntraEntry`, `MixedIntraMacroblocks`,
  `MixedIntraMb`.

- Round 246 (typed `0x3200` per-macroblock walker): new
  `vector::V1OnlyMacroblocks` iterator at `src/vector.rs`
  implementing spec §3.1 of
  `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` — the
  V1-only vector chunk has no flag word, exactly `mb_count`
  payload bytes, each byte a V1 codebook index for one macroblock
  in row-major scan order per spec §1.1.
  `V1OnlyMacroblocks::new(payload, mb_count)` verifies the spec
  §3.1 length-equality invariant up-front (`payload.len() ==
  mb_count`); the resulting iterator is infallible and yields
  exactly `mb_count` `V1MacroblockEntry { index, codebook_index }`
  values before returning `None`. Intended composition: a
  `StripChunkEntry::payload` from round-243's `StripChunks`
  whose `kind` resolves to `VectorChunkKind::IntraV1Only` feeds
  straight into `V1OnlyMacroblocks::new`, so the chunk-stream
  layer (r243) and the per-macroblock layer (r246) share a
  zero-copy contract. The walker is read-only and
  content-agnostic — codebook expansion (spec §4) and pixel
  writes stay in `decoder::decode_strip_chunks`'s hot path.
  `cursor()`, `remaining()`, `mb_count()`, and `payload()`
  accessors round out the typed surface; `Iterator::size_hint`
  and `ExactSizeIterator::len` both report the exact remaining
  count. Tests at `src/vector.rs::tests` (10 added) exercise:
  scan-order index emission, the spec §3.1 fixture `Y6`
  (16-MB strip → 16-byte payload, per-MB-distinct indices), the
  spec §3.1 fixture `Y11` worked example (64-MB strip →
  64-byte payload), an empty-strip happy path
  (`mb_count == 0`), payload-shorter-than-`mb_count` +
  payload-longer-than-`mb_count` rejection at construction,
  `size_hint` exactness through full consumption,
  `ExactSizeIterator::len` honesty, the `cursor` / `remaining`
  advance lock-step contract, and a cross-check that the
  iterator yields the same V1 codebook indices as
  `decode_vector_chunk(0x3200, payload, mb_count)`. New public
  exports from the crate root: `V1MacroblockEntry`,
  `V1OnlyMacroblocks`.

- Round 243 (typed strip-chunk-stream iterator): new
  `codebook::StripChunks` iterator at `src/codebook.rs` implementing
  spec §1 + §2 of `docs/video/cinepak/spec/02-codebooks.md` (common
  4-byte chunk header + codebook chunk taxonomy) plus spec §2 of
  `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` (vector
  chunk taxonomy `0x3000` / `0x3100` / `0x3200`). `StripChunks::new`
  takes a strip-payload byte slice — the `strip_size - 12` bytes
  that follow the 12-byte strip header, equivalently the
  `StripEntry::payload` slice from the round-240 `FrameStrips`
  iterator — and yields one `Result<StripChunkEntry>` per declared
  chunk. Each `StripChunkEntry` carries the 0-based chunk index, the
  classified `StripChunkKind` (codebook vs vector via the new
  `StripChunkKind::from_id` dispatch), the raw 16-bit big-endian
  `chunk_id`, the declared `chunk_size`, and a zero-copy `&[u8]`
  slice covering the `chunk_size - 4` payload bytes. New types
  `VectorChunkKind` (enumerates `IntraMixed` / `InterWithSkip` /
  `IntraV1Only` with bidirectional `from_id` / `to_id`) and
  `StripChunkKind` (sum of `CodebookChunkKind` + `VectorChunkKind`)
  pair this with the existing `CodebookChunkKind` so the chunk
  layer matches the structural depth of the round-240 strip layer.
  Iterator semantics: one-shot fuse on the first malformed chunk
  (truncated 4-byte header, `chunk_size < 4`, payload overrunning
  the strip, or an unrecognised `chunk_id`), then `None` on every
  subsequent call. The iterator is read-only and
  content-agnostic — `apply_codebook_chunk` and the vector chunk
  decoder are left out of the call path — so validators, fuzz
  harnesses that want per-chunk boundaries, and wire-format
  introspection tools can take this single dependency in place of
  the full codebook + vector decode stack. Tests at
  `src/codebook.rs::tests` (13 added) cover: `VectorChunkKind`
  classification + roundtrip; `StripChunkKind` dispatch across the
  full codebook / vector grid; empty-payload happy path; spec §3.4
  fixture `T4` (two header-only codebook chunks + inter vector
  chunk in the natural inter-reuse pattern); spec §3.1 fixture
  `T1a` byte-exact V1 entry slice (`48 48 48 48 db 5a`) +
  `0x3200` V1-only vector chunk pairing; `Σ declared_size ==
  payload.len()` invariant; truncated header, `chunk_size < 4`,
  payload overrun, unrecognised id fuse paths; and a
  partial-walk-then-fuse case that yields two `Ok` chunks before
  the third errors. New public exports from the crate root:
  `StripChunks`, `StripChunkEntry`, `StripChunkKind`,
  `VectorChunkKind`.

- Round 240 (typed strip-header iterator): new `header::FrameStrips`
  iterator at `src/header.rs` implementing
  `docs/video/cinepak/spec/01-frame-and-strip.md` §3 decoder
  algorithm steps 2.1–2.4 for the spec-standard (non-deviant)
  frame layout. `FrameStrips::new(bytes)` parses the 10-byte
  frame header (spec §1) and trims the buffer to `frame_length`;
  `Iterator::next` yields a `Result<StripEntry>` per strip, where
  `StripEntry` carries the 0-based strip index, the resolved
  `StripHeader` (with the spec §2.2 y-coordinate sentinel rule
  applied against a private `prev_y_bottom` accumulator), and a
  zero-copy `&[u8]` slice covering the strip's chunk-stream
  payload (`strip_size - 12` bytes that follow the 12-byte strip
  header). The iterator is read-only — it does not touch
  codebooks, vector chunks, or pixels — so container code
  (e.g. AVI consumers probing per-strip sizes) and validators /
  fuzz harnesses can enumerate strip-level metadata without
  dragging the VQ machinery into scope. Error semantics: one-shot
  fuse on the first malformed strip (`Some(Err(_))` once, then
  `None`); construction rejects buffers shorter than
  `frame_length`. Public accessors: `header() -> &FrameHeader`,
  `remaining() -> u16`, `size_hint() -> (usize, Some(usize))`.
  Deviant streams (Sega FILM Saturn `'cvid'` + Lemmings 3DO
  6-byte prefix per
  `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines
  125–143 and 189) are not in this iterator's scope; those
  continue to use `CinepakDecoder::decode_deviant_frame`.
  Coverage: 10 new tests under `header::tests` (3-strip sentinel
  walk, first-strip literal-coordinate exemption, `O1`
  single-strip pattern, payload-slice length + content contract,
  spec §3 sum-of-payload-lengths invariant, frame-length
  buffer-short rejection at construction time, strip-header
  truncation fuse, strip-size overrun fuse). Pure additive
  change — no behaviour change to the existing decoder, encoder,
  or FILM demuxer code paths.
- Round 234 (FILM PCM-shaping fuzz target): new `film_pcm_decode`
  libFuzzer harness under `fuzz/fuzz_targets/film_pcm_decode.rs`
  driving panic-free coverage across the round-228 PCM helpers —
  `pcm_sign_magnitude_to_i8`, `pcm_decode_8bit` (both
  `TwosComplement` and `SignMagnitude` conventions, exact-length and
  off-by-one destination buffers), `pcm_decode_16be_to_i16`,
  `pcm_deinterleave_stereo_8bit`, `pcm_deinterleave_stereo_16be`,
  plus the `FilmAudioFormat::decode_chunk_to_i16` dispatcher across
  `(8, 1)` / `(8, 2)` / `(16, 1)` / `(16, 2)` LinearPcm cells and
  the `None`-on-unsupported-combo arm (channels ∈ {3, 4}), and the
  early-return path on the non-LinearPcm `FilmAudioFormat::None`
  discriminator. The harness threads one fuzz input through seven
  entry points so a single mutation that exposes a size-arithmetic
  or sign-extension boundary case improves coverage across the
  whole shaping surface in one iteration. Defence-in-depth raw-input
  cap of 64 KiB matches the sibling targets; every helper allocates
  proportional to input length so worst-case destination buffers are
  bounded at 64 KiB (8-bit paths) / 128 KiB (16-bit `Vec<i16>`
  paths). Closes the natural follow-up to the round-228 helper
  landing — the `film_demuxer_parse` target covers the container
  parse but exits before any audio payload reaches the helpers; the
  `decode_frame` family targets the video bitstream only. Local
  9-second smoke run: 1.5M libFuzzer iterations, 427 coverage
  points, 1310 features, 178-unit corpus, zero panics. Pure additive
  change — no `src/` behaviour change, no Cargo.toml `[package]`
  changes; only the fuzz sub-package's `[[bin]]` table gains a new
  entry.
- Round 228 (FILM linear-PCM sample-data shaping helpers): new free
  functions on the `film` module + a convenience accessor on
  `FilmAudioFormat`, closing the natural follow-up to the round-221
  audio classifier. Round 221 surfaced the metadata for FILM PCM
  payloads — channels, bits-per-sample, sample rate, big-endian byte
  order, twos-complement vs sign/magnitude convention — but left the
  documented "consumer is responsible for re-interleaving before
  passing to a typical PCM playback API" gap open. Round 228 closes
  it. `pcm_sign_magnitude_to_i8(b: u8) -> i8` converts one
  sign/magnitude byte to host two's-complement per
  `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 163–169
  (`0x81` ⇒ `-1`, `0xFF` ⇒ `-127`; `0x80` "negative zero" collapses
  to `0i8`). `pcm_decode_8bit(src, convention, dst)` decodes a slice
  with either `TwosComplement` (byte-for-byte bitcast) or
  `SignMagnitude` (per-byte rule) per wiki line 151 vs 162.
  `pcm_decode_16be_to_i16(src, dst)` converts 16-bit big-endian PCM
  to host `i16` per wiki line 153. `pcm_deinterleave_stereo_8bit(src,
  dst)` and `pcm_deinterleave_stereo_16be(src, dst)` re-interleave
  FILM's documented L-then-R half-chunk stereo layout (wiki lines
  156–160) into standard `L R L R …` form; the 16-bit helper folds
  in the big-endian decode in one pass. One-shot convenience:
  `FilmAudioFormat::decode_chunk_to_i16(src) -> Option<Vec<i16>>`
  dispatches on the `LinearPcm` discriminator across the four
  documented `(bits_per_sample, channels)` combos (mono / stereo ×
  8-bit / 16-bit), returning `None` for non-`LinearPcm` variants,
  unsupported combos (`channels` not in `{1, 2}`, `bits_per_sample`
  not in `{8, 16}`), and source-length / channel-count mismatches.
  The 8-bit path sign-extends each `i8` to `i16` (no scaling to the
  full 16-bit range — that's a remixing decision left to the
  caller). 30 new tests in `tests/r228_film_pcm_shaping.rs` cover
  the verbatim wiki examples (`0x81`/`0xFF`/`0x01`/`0x7F`), `0x00`
  vs `0x80` "negative zero" collapse, total-function coverage of
  every input byte, twos-complement bitcast vs sign-magnitude rule,
  big-endian round-trip across the full `i16` range, channel
  re-interleave correctness, sample preservation across the
  re-shuffle, every length / size precondition (odd length,
  non-multiple-of-four, dst-size mismatch), each
  `FilmAudioFormat::decode_chunk_to_i16` dispatch path with both
  Saturn and Sega CD sign-convention rules, the `None`-on-non-PCM
  branch (`CriAdxAdpcm` / `Unknown` / `FilmAudioFormat::None`), the
  `None`-on-unsupported-combo branch (4-channel, 24-bit), and empty
  / zero-length inputs. Pure additive change — no demuxer / decoder
  / encoder behaviour change, no Cargo.toml changes. PCM playback
  (rate conversion, channel mixing, gain) and ADX ADPCM decoding
  remain out of scope per `docs/video/cinepak/spec/00-scope.md`;
  these helpers only re-shape the documented FILM wire bytes into
  the format a generic PCM sink expects (twos-complement,
  host-endian, channel-interleaved).
- Round 221 (FILM container audio-format classifier): new
  `FilmAudioFormat` enum (`None` / `LinearPcm { channels,
  bits_per_sample, sample_rate_hz, endianness, sign_convention }` /
  `CriAdxAdpcm { channels, sample_rate_hz }` / `Unknown { … }`),
  `PcmEndianness` (`BigEndian` for 16-bit per `Sega_FILM.wiki` line
  153, `NotApplicable` for 8-bit), and `PcmSignConvention`
  (`TwosComplement` for Saturn ASCII versions per wiki line 151,
  `SignMagnitude` for Sega CD / 3DO NULL versions per wiki line 162
  + 224) classify the FDSC audio fields per
  `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 147–169.
  Accessors: `Fdsc::has_audio()` / `Fdsc::audio_format(&version)` /
  `FilmDemuxer::audio_format()` /
  `FilmDemuxer::audio_duration_seconds()` (sum of audio-sample
  `sample_length` ÷ linear-PCM byte rate; returns `None` for non-PCM
  compression or any zero-rate field) /
  `FilmAudioFormat::byte_rate_bps()` (defined for `LinearPcm` with
  `bits_per_sample` a non-zero multiple of 8) /
  `FilmAudioFormat::is_linear_pcm()`. Defensive validation:
  `SampleRecord::is_well_formed_audio()` enforces wiki line 116's
  "audio sample_info_2 is always 1" verbatim; companion
  `FilmDemuxer::first_malformed_audio_sample()` returns the index of
  the first offender (or `None` if all audio rows are well-formed).
  14 new tests cover the no-audio sentinel + any-field-set
  detection, Saturn 16-bit stereo big-endian twos-complement, Saturn
  8-bit `NotApplicable`-endianness, Sega CD NULL-version
  sign/magnitude inference, the ADX ADPCM branch, the
  unknown-compression preservation, byte-rate edge cases (zero
  fields, non-multiple-of-8 bits), audio-duration summation +
  no-records / non-PCM fallback to `None`, the `sample_info_2`
  validator, the first-malformed pinpoint, the classifier's
  independence from video FDSC fields, and the abbreviated-FDSC
  no-audio rule. Audio codec decoding (PCM playback, ADX ADPCM
  decompression) stays out of scope for this crate per
  `docs/video/cinepak/spec/00-scope.md`; the new helpers surface
  the wire metadata so consumers can route the bytes returned from
  `FilmDemuxer::audio_samples()` to a PCM / ADPCM sink with the
  correct byte order, sign convention, and byte rate. Pure additive
  change — no encoder / decoder touches, no Cargo.toml changes.
- Round 215 (vintage-decoder-compatible encoder mode): new
  `EncoderOptions::vintage_compat: bool` (default `false`). When
  `true`, enforces two structural constraints from `Cinepak.wiki`
  line 33 quoted in `docs/video/cinepak/spec/01-frame-and-strip.md`
  §2.3 and `02-codebooks.md` §2.2 / §3.4: (i) `validate_opts`
  rejects `strip_count > 3` with a clear "vintage_compat requires
  strip_count ≤ 3" message (vintage Windows + MacOS players reject
  frames above the 3-strip ceiling); (ii) inter strips that would
  otherwise chunk-omit (0 wire bytes, decoder inherits previous
  codebook per spec §3.4) instead emit a **header-only chunk**
  (`chunk_size = 0x0004`, 4 wire bytes) so each strip's chunk
  stream always carries both V4 then V1 codebook chunks in strict
  V4-then-V1 order — the shape vintage MacOS Cinepak players insist
  on. Header-only and chunk-omitted forms are decoder-equivalent —
  both signal "inherit previous codebook" — so a vintage-compat
  encoded stream self-decodes to byte-identical pixels as the same
  input encoded with the default path. The intra path is already
  conformant (always emits V4-then-V1 full-replace per strip), so
  the flag is a no-op on intra-only sequences. Wire-size overhead
  caps at `2 × 4 × strips × inter_frames` bytes (one chunk header
  per otherwise-omitted chunk × V4+V1 per strip). 9 new tests
  cover the strip-count ceiling (accept-3 / reject-4 with vs
  without the flag), the header-only chunk wire shape and
  V4-then-V1 ordering on inter strips, the bounded wire-size
  overhead, intra-path no-op equivalence, every-inter-strip
  carries-two-chunks across a multi-strip frame, and the
  decode-equivalence pixel check. Vintage support is encoder
  policy, not a bitstream feature — output stays conformant
  Cinepak in both modes.
- Round 209 (depth-mode profiling extension): two new modes on
  `examples/profile_cinepak.rs` — `picker` sweeps the four public RGB
  encoder entry points (`encode_rgb24` baseline →
  `encode_rgb24_round6` 2-axis → `encode_rgb24_round7` 3-axis →
  `encode_rgb24_round8` per-strip picker) on each scenario, reporting
  per-entry wall time + output size + trial-count ceiling so the
  wire-vs-time cost of each picker upgrade is visible side-by-side;
  `samples=N` argument makes every per-scenario row report median +
  (max-min)/median jitter across `N` independent rep groups so single-
  sample noise (±30 % on a contended laptop) doesn't bury real
  per-pass deltas. Existing round-160 CLI invocations
  (`profile_cinepak encode 20`, etc.) stay byte-compatible — `samples`
  defaults to 1 which preserves the original code path. New
  `profile/README.md` section captures the picker-axis cost
  progression (baseline → round-6 ≈ 8×, round-6 → round-7 ≈ 3×,
  round-7 → round-8 ≈ 3×) and the encode/decode jitter floor
  (decode ≤ 3 %, encode 1–10 % depending on fixture size) so future
  A/B rounds know the signal threshold before reporting "no
  regression". Pure additive change to the round-160 driver — no
  src/ touches, no dep / Cargo.toml changes.
- Round 202 (per-parser fuzz corpora): named seed files for the three
  fuzz targets that landed in r181 / r192 / r175 without committed
  corpora. `examples/seed_fuzz_corpora.rs` writes deterministic seeds
  under `fuzz/corpus/{codebook_chunk_apply,decode_vector_chunk,
  decode_deviant_frame}/`: 18 codebook seeds covering every chunk-id
  leaf in the 8-code V4/V1 × Full/Selective × YUV/Gray family with
  both `tolerate_trailing` settings + header-only + truncated seeds;
  6 vector seeds covering V1-only intra, mixed-intra at one + two
  flag-word groups, inter all-skip, inter with the
  `inter_payload_straddle` selector-spillover pattern, and an
  unknown-id 0x0000 reject seed; 3 deviant seeds covering Saturn /
  Lemmings-3DO deviant frames + a standard-control 4×4 frame. New
  `tests/seed_fuzz_corpora.rs` integration test drives every seed
  through the same public entry points the fuzz harnesses invoke and
  asserts the expected positive-arm counts so a wire-surface
  refactor trips `cargo test` instead of fuzz CI. Mirrors the r196
  `decode_multi_frame` seeding pattern.
- Round 196 (`decode_multi_frame` fuzz target): eighth entry in the
  `cargo-fuzz` harness. Drives a single `CinepakDecoder` instance over
  a sequence of length-prefixed frame slices (u16 BE per-frame length,
  capped at 8 frames per input), exercising the inter-frame state
  machine — rolling V1/V4 codebooks across `decode_frame` calls,
  selective-update codebook chunks inheriting from prior strips, and
  `0x3100` SKIP macroblocks copying out of the previous frame's
  reconstructed raster. The single-frame `decode_frame` target
  instantiates a fresh decoder per input and therefore never reaches
  the selective-update arithmetic against a non-empty `prev_v4` /
  `prev_v1`, the skip-copy from a `prev_frame` whose dims / pixel mode
  were chosen by the fuzzer, or the intra-after-inter transition that
  must wipe inheritance even with prev_frame state present. Mirrors
  the per-frame 256 × 256 coded-pixel cap from `decode_frame.rs` plus
  the 64 KiB raw-input cap from the sibling targets. Seeded with two
  encoder-round-tripped multi-frame streams (intra + 1 inter, intra +
  2 inters) at 32 × 32. 60-second local run: 7.94 M executions,
  130 095 execs/s, 1 415 new corpus units from the two seeds, no
  crashes.

## [0.0.2](https://github.com/OxideAV/oxideav-cinepak/releases/tag/v0.0.2) - 2026-05-30

### Other

- round 192: decode_vector_chunk fuzz target
- round 187: FILM demuxer seek-friendly helpers
- round 181: codebook_chunk_apply fuzz target
- round 175: decode_deviant_frame fuzz target
- round 160: standalone profiling driver + captured baseline
- round 155: ffmpeg multi-frame inter cross-decode + frame-header flags fix
- round 148: cargo-fuzz harness + 2 fuzz-found subtract-with-overflow fixes
- round 143: seek-friendly keyframe interval enforcement
- round 134: refresh README decode-bench rows to the post-r129 decoder
- round 129: decoder hot-path rewrite — −17%..−67% per-frame time vs r126 baseline
- round 126: criterion benches for decoder + encoder picker tiers + roundtrip
- round 121: chroma CBR convergence via carry-over accumulator
- round 113: target-bitrate rate control on the grayscale stateful inter path
- round 104: inter-frame grayscale encode path + decoder mode-hint fallback
- round 101 grayscale RD-grid frame-level picker (Lever N)
- round 96 — bitrate-target per-frame rate control
- Sega Saturn / Lemmings 3DO deviant Cinepak decoder
- round 9 — k-means++ initialisation for cold-start codebook training (Lever M)
- round 8 — per-strip independent (lambda, luma_weight) picker (Lever L)
- round 7 — 3-axis RD grid picker + Y-channel scoring (Levers J + K)
- round 6 — post-classification Lloyd polish + RD grid picker (Levers H + I)
- round 5 — luma-weighted distance + luma-prioritized median-cut split (Levers F + G)
- round 4 — Linde-Buzo-Gray (LBG) split refinement (Lever E)
- round-47 encoder PSNR_Y win — Lagrangian V1/V4 RDO + strip picker
- round 7 — empty-cluster slot reclamation + adaptive bisection tolerance
- round 6 — windowed bisection rate ctrl + tighter Lloyd + ffmpeg AVI roundtrip
- round 5 — cross-frame codebook persistence + multi-strip selective + two-pass rate control
- round 4 — selective-update codebook chunks on inter strips
- round 3 — multi-strip + inter encoder + quality knob + ffmpeg PSNR floor
- round 2 — encoder + Sega FILM demuxer + CVID probe
- round 1 — Implementer: full decoder from clean-room spec
- Round 0 — clean-room rebuild scaffold (orphan master)

### Added

- Round 192 (`decode_vector_chunk` fuzz target): seventh entry in
  the `cargo-fuzz` harness, driving `vector::decode_vector_chunk`
  directly so libFuzzer's mutations don't have to thread through a
  frame header + strip header + chunk header before they reach the
  vector parser. The new target exercises all three vector codes
  (`0x3200` V1-only intra, `0x3000` mixed V1/V4 intra with per-32-MB
  flag-word groups, `0x3100` inter with skip codes and bit-grammar
  selector-bit straddle across flag-word boundaries) on every input:
  the dispatcher's `chunk_id` branch is fuzzed, plus three explicit
  re-decodes under each known code so each sub-decoder gets its own
  coverage signal. `mb_count` is capped at 4096 to bound the
  pre-allocated `Vec<Mb>` even when the payload is rejected. Raw
  input is capped at 64 KiB like the sibling structural-parse
  targets. Source: spec §3 (`docs/video/cinepak/spec/03-vectors-and-macroblocks.md`).
- Round 187 (FILM demuxer seek-friendly helpers): six new public
  accessors on `FilmDemuxer` + `SampleRecord` that close the
  long-standing "FILM parser exposes only the raw sample table, not
  the seek primitives a player needs" gap. The wire format already
  carries everything required for keyframe-aware seek per
  `Sega_FILM.wiki` lines 104, 110-116 (sample_info_1 top bit ⇒
  keyframe, lower 31 bits ⇒ absolute tick timestamp,
  sample_info_2 ⇒ ticks-until-next-frame, STAB.base_frequency ⇒
  ticks/sec); round 187 surfaces it through:
  - `SampleRecord::next_frame_ticks() -> Option<u32>` —
    `sample_info_2` exposed with the video-only semantics from
    wiki line 116 ("the number of clock ticks until the next frame
    is rendered"). Returns `None` for audio records where the field
    is hardcoded to `1` and carries no inter-frame-gap meaning.
  - `FilmDemuxer::audio_samples()` — mirror of the existing
    `video_samples()`; iterates only records whose
    `sample_info_1 == 0xFFFFFFFF`. Useful when routing audio bytes
    to a separate decoder while skipping the interleaved video.
  - `FilmDemuxer::keyframes()` — iterates only video keyframes in
    sample-table order. Wiki line 104 explicitly motivates this:
    "useful for seeking since it is a good idea to only jump to
    key frames when seeking through a file".
  - `FilmDemuxer::seek_keyframe_for_tick(target_ticks: u32)` —
    snap-to-keyframe seek primitive. Returns the
    `(sample_index, &SampleRecord)` for the keyframe whose
    timestamp is the **largest value ≤ target_ticks**, the
    canonical seek-floor operation any FILM player needs. Tolerant
    of non-timestamp-sorted sample tables (linear O(n) scan over
    the keyframe subset; Cinepak's CD-era frame counts make the
    scan cost trivial). Returns `None` only when no keyframes meet
    the floor (e.g. seeking before the first keyframe, or a
    pathological all-inter table).
  - `FilmDemuxer::duration_ticks() -> Option<u32>` — total stream
    duration in `base_frequency` ticks, computed as
    `max(ts) + next_frame_ticks(last)` per wiki line 116 — the
    natural "end of last frame's render window" definition.
    Returns `None` for video-free files (audio-only).
  - `FilmDemuxer::duration_seconds() -> Option<f64>` — convenience
    division by `StabHeader::base_frequency`. Returns `None` on
    zero base frequency (defence against a degenerate input the
    parser doesn't currently reject).
  10 new unit tests in `src/film.rs::tests` cover: video-only
  `next_frame_ticks` semantics; `audio_samples` index correctness on
  an interleaved 4-record table; `keyframes` ignoring both inter
  frames and audio sentinels (defence-in-depth on the `is_audio`
  guard inside the iterator); `seek_keyframe_for_tick` snapping
  at, before, between, exactly on, and past the last keyframe;
  the `None` return for all-inter pathological tables; the linear
  scan handling non-sorted sample tables; `duration_ticks` against
  both even-spaced (30 fps @ 600 Hz) and variable-gap
  (Myst-style 30 Hz with 2/3-tick alternation per wiki line 114)
  fixtures; and the `duration_seconds` zero-base-frequency guard
  returning `None` while `duration_ticks` still computes a value.
  Seek primitives are framework-feature-free — the
  `default-features = false` standalone build picks them up too.

- Round 181 (codebook-chunk-apply fuzz target): new
  `fuzz/fuzz_targets/codebook_chunk_apply.rs` panic-free libFuzzer
  target plus matching `[[bin]]` entry in `fuzz/Cargo.toml`. Drives
  `codebook::apply_codebook_chunk` and `apply_codebook_chunk_with`
  (the Sega Saturn deviant `tolerate_trailing` sibling, per
  `docs/video/cinepak/spec/02-codebooks.md` §3.4) directly with
  arbitrary `chunk_id` + payload pairs, instead of routing all
  codebook-parser mutations through the existing `decode_frame` /
  `decode_deviant_frame` targets that have to thread through a
  frame-header + strip-header + chunk-header first. Sweeps the
  2 × 2 × 2 grid of codebook chunk-type codes (V4 vs V1, full
  replacement vs selective update, 12-bit YUV vs 8-bit grayscale —
  spec §2 / §2.1) per input by flipping the bit-2 grayscale
  selector after the strict pass to exercise both stride
  arithmetics (6 B vs 4 B entries) and both `apply_full` /
  `apply_selective` paths under both strict + tolerate-trailing
  modes. Per-target memory pressure stays bounded by the
  `Codebook::default()` 256-entry × ≤ 6 B fixed footprint
  (≈ 1.5 KiB × ≤ 3 codebooks per input); the standard
  `MAX_INPUT_BYTES = 64 KiB` defence-in-depth cap matches the
  sibling targets. The reusable fuzz workflow auto-discovers
  `fuzz/fuzz_targets/*.rs`, so the new file is picked up
  automatically with the daily budget now split across six
  targets instead of five.

- Round 175 (deviant-frame fuzz target): new
  `fuzz/fuzz_targets/decode_deviant_frame.rs` panic-free libFuzzer
  target plus matching `[[bin]]` entry in `fuzz/Cargo.toml`. Closes the
  coverage gap left by round 148: the prior `decode_frame` target only
  exercises the strict standard path, so the Saturn / Sega CD `'cvid'`
  and Lemmings 3DO branches in `CinepakDecoder::decode_deviant_frame`
  (`src/decoder.rs:136`) were not reached by any fuzz budget. The new
  target loops over the three documented `DeviantConfig` permutations
  per input — `saturn()` (2-byte prefix, frame_length short by 8,
  codebook pad tolerated), `lemmings_3do()` (6-byte prefix, same
  short-by, same pad), and a `decode_frame` standard-path control —
  so libFuzzer can shape mutations against the specific
  deviant-vs-standard branches: `extra_header_bytes`,
  `frame_length_short_by`, and `tolerate_codebook_pad`. Same
  `MAX_CODED_PIXELS = 256 × 256` and `MAX_INPUT_BYTES = 64 KiB` caps
  as the existing `decode_frame` target, so the per-frame raster
  allocator and libFuzzer corpus shaping behave identically across
  the standard and deviant harnesses. The reusable fuzz workflow
  auto-discovers `fuzz/fuzz_targets/*.rs`, so the new file is picked
  up automatically with the daily budget now split across five
  targets instead of four.

- Round 160 (depth-mode profiling driver): new
  `examples/profile_cinepak.rs` driver — a flat measure-this-thing
  harness that builds deterministic xorshift32-seeded RGB / grayscale
  inputs once then runs a fixed iteration count of the four
  cost-axis scenarios the round-126 Criterion benches target
  (`rgb24/64x64/q50/round7`, `rgb24/320x240/q50/baseline`,
  `rgb24/640x480/q70/baseline`, `gray8/320x240/q50/baseline`) plus a
  `stateful` mode that drives a 5-frame GOP through the full picker
  + rolling-codebook + rate-control machinery the stateless
  `encode_*` entry points bypass. The driver is intended for
  sampling-profiler capture (`samply` on macOS, `perf record` on
  Linux, `cargo flamegraph` on either) without the warm-up + sampler
  layers Criterion adds between iterations. Modes:
  - `encode` — synth pixels, encode N times (decoder cost excluded)
  - `decode` — encode each scenario once outside the loop, decode N
    times against the cached bytes
  - `roundtrip` — synth pixels, encode + decode every iteration
  - `stateful` — drive a 5-frame GOP (intra + 4 inter), measuring
    the picker / rate-control cost as it appears in a real streaming
    use case
  - `all` — run every mode (default)
  Captured baseline numbers (Apple M4 Max, release build) committed
  alongside the driver under `profile/README.md`: decode peaks at
  4.4 GiB/s of raw output on the 640×480/q70 fixture (~3000×
  realtime at 320×240@30); stateless encode runs at 24 ms/iter on
  320×240/q50 (~9.1 MiB/s of raw input); the stateful 5-frame GOP at
  320×240/q50 averages 13.5 ms/frame across 1 intra + 4 inter. The
  baseline is the durable artefact — future optimisation rounds
  re-run the driver against the same scenarios to A/B-compare their
  changes, with regression > 10 % on any row the bisect trigger.

### Fixed

- Round 155 (frame-header `flags` byte on inter frames): the encoder
  previously hardcoded `flags = 0x00` in every frame header it
  emitted, including inter frames where spec
  `docs/video/cinepak/spec/01-frame-and-strip.md` §1.1
  (lines 88–99) + `02-codebooks.md` §5.2 require `flags & 0x01` set
  on the inter frame to advertise codebook inheritance from the
  previous strip / previous frame's last strip. Our own decoder is
  permissive and inherits unconditionally (which is why no in-crate
  test caught the bug), but ffmpeg's decoder is strict about the bit:
  a `flags = 0x00` inter frame carrying selective-update (`0x2100` /
  `0x2300`) or chunk-omitted strips would decode against an
  uninitialised codebook on ffmpeg's side, producing visible drift on
  multi-frame inter sequences. The fix:
  - `assemble_frame` now takes an explicit `flags: u8` parameter.
  - Intra call sites (stateful `encode_intra_one`, stateless
    `encode_intra_frame`, and the `encode_rgb24_per_strip_rd` /
    `_round8` intra picker) pass `0`.
  - Inter call sites (stateful `encode_inter_one`, stateless
    `encode_inter_frame`) pass `0x01`.
  Single-frame self-decode tests are unaffected; the new
  `tests/ffmpeg_multi_frame_inter.rs` covers the cross-decode
  conformance for a single-GOP 5-frame sequence (≥ 28 dB per frame
  against ffmpeg 8.1).

### Added

- Round 155 (multi-frame stateful-inter cross-decode against ffmpeg):
  `tests/ffmpeg_multi_frame_inter.rs` exercises a 5-frame 96×64
  single-GOP RGB24 sequence — encodes with
  `CinepakEncoder::encode_intra` + `encode_inter`, wraps in a
  multi-frame AVI with proper per-frame keyframe flags + multi-entry
  `idx1`, and pipes through ffmpeg as a black-box decoder. Asserts
  per-frame PSNR ≥ 28 dB versus the synthetic source and reports a
  mean PSNR of 34.18 dB on the local fixture. A companion
  `self_multi_frame_inter_decode` runs the same sequence through our
  own decoder and asserts the same floor, providing a fixture-driven
  regression even when ffmpeg is unavailable on the CI runner. The
  test gracefully skips (with `eprintln!` + early return) when
  `ffmpeg` is absent or `OXIDEAV_SKIP_FFMPEG_TESTS` is set.

- Round 148 (decoder cargo-fuzz harness): new `fuzz/` sub-package
  with four panic-free libFuzzer targets — `frame_header_parse`,
  `strip_header_parse`, `decode_frame`, `film_demuxer_parse` — and a
  `.github/workflows/fuzz.yml` shim around the org-level
  `crate-fuzz.yml` reusable workflow (1800 s daily budget split
  across the four). `decode_frame` peeks at the wire `width`/`height`
  (bytes 4..8) and caps coded pixels at 256 × 256 before invoking the
  decoder, so a wire-legal `u16 × u16` raster (worst case ~12 GiB
  RGB24) can't OOM the runner. The structural parse harnesses cap raw
  input at 64 KiB.

### Fixed

- Round 148 (fuzz-found subtract-with-overflow bugs in the strip
  header path):
  - Decoder now rejects strips with wire `x_top > x_bottom` before
    the modulo-4 alignment check underflows `sx1 - sx0`. New unit
    test `decoder::tests::rejects_strip_with_x_top_above_x_bottom`.
  - Decoder now rejects strips with resolved
    `actual_y_top > actual_y_bottom` before the chunk loop reads
    `StripHeader::height()`. The `height()` accessor itself also
    moved to `saturating_sub` as defence-in-depth. New unit test
    `decoder::tests::rejects_strip_with_y_top_above_y_bottom`.

  Both bugs surfaced within the first ~10 seconds of fuzzing on the
  fresh `decode_frame` target. A follow-up 90-second
  `cargo fuzz run decode_frame` after both fixes (~19.7 M execs,
  cov 53, ft 53) found no further crashes.

- Round 143 (seek-friendly keyframe interval enforcement):
  `CinepakEncoder::with_keyframe_interval(n)` /
  `set_keyframe_interval(n)` / `clear_keyframe_interval()` /
  `keyframe_interval()` / `gop_position()` / `force_next_keyframe()` plus
  new `encode_frame(rgb, w, h, opts) -> EncodedFrame` and
  `encode_frame_gray8(input, w, h, opts) -> EncodedFrame` auto-routing
  entry points (and the public `EncodedFrame { bytes, is_keyframe,
  frame_number_in_gop }` struct). Configure an interval and the
  encoder dispatches each frame to `encode_intra` / `encode_inter`
  automatically — frame `0`, `n`, `2n`, … are intra, the rest are
  inter — and returns the encoded bytes together with the
  `is_keyframe` flag the container muxer needs to set the container's
  keyframe sample bit (AVI `AVIF_KEYFRAME`, QuickTime sync sample,
  Sega FILM `sample_info_1`). `force_next_keyframe()` requests a
  one-shot intra refresh that overrides the schedule (useful for
  scene-cut detection feeding back from
  `last_rate_stats().byte_delta > 0` overshoot signalling, or from
  upstream scene-detect); the GOP counter resets at the forced
  keyframe so subsequent keyframes are interval-spaced from the
  forced one. The router also defensively re-keyframes on a pixel
  mode switch (`Rgb24` ⇒ `Gray8` or vice versa) so the inter
  worker's grayscale-after-colour rejection can't trip an
  auto-routed sequence. Composes with all existing levers: the
  budget-driven worker (round 96 / 113 / 121) continues to drive
  the RD grid toward the configured per-frame budget, cross-frame
  persistence (round 5) and slot reclamation (round 7) carry across
  inter frames unchanged, and `reset()` preserves the configured
  interval (it's a configuration knob, not per-sequence state) while
  zeroing the GOP counter so the next call is a clean intra.
  `keyframe_interval = None` (the default) preserves the legacy
  manual-routing behaviour exactly — `encode_intra` / `encode_inter`
  callers see no change, and `encode_frame` returns a clear "set the
  interval first" error in that mode.

  Spec reference: `docs/video/cinepak/spec/01-frame-and-strip.md`
  §1.1 (container keyframe flag ⇔ codec `flags = 0x00` ⇔ all strips
  `0x1000` intra; the auto-router's `is_keyframe` flag agrees with
  the first strip's `strip_id` on the wire per §2.1, the conformant
  intra/inter dispatch signal). Rate control / GOP scheduling are
  encoder policy, not bitstream features (per
  `00-scope.md` §"Lossy-codec validation criterion"); output stays
  conformant Cinepak. 13 new tests cover error-without-interval,
  the period pattern at intervals 1 / 5, zero-interval clamping to
  1, `force_next_keyframe` schedule reset, force one-shot semantics,
  `reset` preserving the interval while clearing the GOP counter,
  `clear_keyframe_interval` returning to manual mode, end-to-end
  decode of an interval-3 / 10-frame sequence, the grayscale path
  period pattern, the mode-switch defensive intra, the
  `gop_position()` accessor tracking the router, and composition
  with `with_target_bitrate` round-96 rate control.

### Changed

- Round 129 (decoder hot-path optimisation): **`CinepakDecoder::decode_frame`
  rewrites four pieces of the per-MB render hot path so the decoder
  benches (`benches/decode.rs`, round-126) drop -17 to -23 % across the
  RGB scenarios and -66.8 % on the grayscale path** versus the round-126
  baseline (`cargo bench --bench decode -- --save-baseline pre` then
  `--baseline pre`). Indicative numbers (Apple M-class, single thread,
  3 s measurement window): `decode rgb24 320×240 q=50` 78.4 µs → 62.7 µs
  (+25 % throughput; now 3.42 GiB/s), `decode rgb24 64×64 q=50` 4.09 µs
  → 3.11 µs (+31 %; now 3.68 GiB/s), `decode rgb24 640×480 q=70` 256 µs
  → 212 µs (+20 %; now 4.04 GiB/s), `decode gray8 320×240 q=50` 77.3 µs
  → 25.7 µs (+201 %; now 2.79 GiB/s — close to parity with the RGB
  path), `decode rgb24 320×240 inter-allskip` 140 µs → 115 µs (+22 %).

  Four independent levers:

  1. **Per-pixel `PixelMode` match hoisted out of the inner loop** —
     `render_strip` now splits into `render_strip_rgb` /
     `render_strip_gray` at the top of the function. Each macroblock
     writes through a mode-specialised draw helper (`draw_v1_mb_rgb` /
     `draw_v1_mb_gray` / `draw_v4_mb_rgb` / `draw_v4_mb_gray`) instead
     of dispatching `PixelMode` per pixel inside `write_pixel`. On a
     64×64 frame that's ~1 200 fewer match dispatches per frame; on
     640×480, ~76 800 fewer.

  2. **V1 macroblock collapsed from 16 to 4 `yuv_to_rgb` calls** — V1
     entries share one `(U, V)` across the whole 4×4 macroblock and
     have only 4 distinct Y samples (one per 2×2 quadrant). The new
     `draw_v1_mb_rgb` builds 4 RGB triples once, packs them into two
     12-byte row templates (Y0 Y0 Y1 Y1 / Y2 Y2 Y3 Y3), and writes
     each row template into the upper and lower halves of its
     quadrant with `copy_from_slice` — replacing the original
     16-call-per-MB inner loop.

  3. **Direct grayscale buffer allocation** — when the first strip's
     chunk pins `PixelMode::Gray8` the output `Vec<u8>` is truncated
     in place from `width × height × 3` to `width × height` and the
     stride is updated. This eliminates the post-decode byte-by-byte
     compaction scan (`for row { for col { gray.push(out[base + col*3]) }}`)
     that dominated the grayscale-path benchmark — going from
     ≈ 947 MiB/s to ≈ 2.79 GiB/s.

  4. **Codebook `clone()` eliminated** — per-strip
     `self.prev_v4.clone().unwrap_or_default()` (a 1.5 KiB memcpy per
     codebook × 2 codebooks × N strips) replaced with
     `self.prev_v4.take().unwrap_or_default()` + a single put-back at
     the end of the strip loop. For a 3-strip frame that's ~9 KiB of
     pointless copies per frame.

  No behavioural change: all 66 + 8 + 3 + 7 + 7 + 6 + 8 + 5 + 5 + 6 + 8
  + 5 + 8 synth-decode / encoder / roundtrip tests stay green. The
  draw helpers retain the same 4×4 layout (`docs/video/cinepak/spec/03-vectors-and-macroblocks.md`
  §4 / §5 unchanged); the grayscale buffer path retains identical
  output bytes (verified by `grayscale_v1_only_intra_8x8` and the
  multi-strip + inter grayscale tests).

### Added

- Round 126 (depth-mode benchmarks): three `criterion` bench harnesses
  (`benches/decode.rs`, `benches/encode.rs`, `benches/roundtrip.rs`)
  covering the cinepak decoder hot paths, the encoder picker tiers
  (baseline / round-6 / round-7 / grayscale round-7), and the stateful
  `CinepakEncoder` + `CinepakDecoder` intra+inter roundtrip path. Each
  bench is self-contained — inputs are synthesised on-the-fly with a
  deterministic xorshift32 gradient so no fixture files are committed
  and the run is reproducible across machines. `criterion` is added as
  a dev-only dependency (`[dev-dependencies] criterion = "0.5"`) and
  three `[[bench]]` entries with `harness = false` register the
  binaries.

  Per the workspace "saturated codecs grow fuzz / bench / profile"
  memo: cinepak is at ≈ 96 % decoder coverage / ≈ 98 % encoder
  coverage after r121, so the next coherent work is making the
  encoder's many picker tiers (rounds 47, 4, 5, 6, 7, 8, 9, 47, 101)
  measurable — not adding another lever to a heap of 12. With
  `criterion` wired up future "Lever N+1" PRs can diff per-frame
  encode cost against the round-9 baseline rather than relying on
  one-shot wall-clock measurements in test output.

  Indicative release-mode numbers (Apple M-class, single thread,
  `cargo bench -- --quick`): decoder ≈ 2.7..3.3 GiB/s on RGB intra,
  ≈ 930 MiB/s on grayscale; `encode_rgb24` 64×64 q=50 baseline
  ≈ 1.4 ms, `encode_rgb24_round6` ≈ 11 ms (× 8 baseline),
  `encode_rgb24_round7` ≈ 31 ms (× 23 baseline) — matching the picker
  trial-encode counts (9 / 27) within constant factors.

- Round 121 (chroma CBR convergence): **CBR carry-over accumulator threads
  surplus and deficit across the chroma (`encode_intra` / `encode_inter`)
  budget-driven sequence so a 5-second clip's total bytes converge to
  `bits_per_second / 8 × duration_s`** within ±10 % on representative
  content, rather than systematically under-shooting the way the round-96
  per-frame cap did on every prior fixture (each frame's leftover budget
  was discarded). The mechanism is a leaky bucket: each budget-driven
  frame's effective budget is `base_budget + clamp(cumulative_target −
  cumulative_actual, 0, cap)`, with `cap = 8 × base_budget` installed by
  default when `set_target_bitrate` / `set_target_frame_bytes` is called
  (overridable via `set_carry_over_cap_bytes` / `clear_carry_over_cap_bytes`).
  Deficits (post-overshoot frames) propagate without a cap — the controller
  fully claws back overshoots on subsequent frames.

  The picker selection rule is unchanged (still "highest-quality candidate
  that fits the effective budget", smallest grid candidate on overshoot,
  never errors on rate). What changed is the budget the picker sees per
  frame: with carry-over, an under-spent frame leaves quality to be paid
  forward to a complex frame later in the sequence, so the multi-frame total
  tracks the bitrate target. The accumulator is shared between the chroma
  and the round-113 grayscale path (both go through `encode_budget_frame`)
  — grayscale benefits from the same convergence guarantee for free, no
  separate code path.

  Six new fields on `RateStats` (the per-frame telemetry struct):
  `effective_budget_bytes`, `effective_byte_delta`,
  `within_effective_budget`, `cumulative_target_bytes`,
  `cumulative_actual_bytes`. Three new `CinepakEncoder` accessors:
  `cumulative_target_bytes()`, `cumulative_actual_bytes()`,
  `carry_over_cap_bytes()`. Three new mutators:
  `set_carry_over_cap_bytes(n)`, `with_carry_over_cap_bytes(n)`,
  `clear_carry_over_cap_bytes()`, `reset_rate_carry_over()`. `reset()` now
  zeros the accumulator (it's per-sequence state), preserves the budget
  and the cap (configuration). `clear_target_bitrate()` zeros everything.

  Rate control is encoder policy, not a bitstream feature — decoded output
  stays conformant Cinepak. The carry-over heuristic is derived from first
  principles (no external encoder source consulted). Reference:
  `docs/video/cinepak/spec/00-scope.md` §"Lossy-codec validation criterion"
  notes encoder-internal rate control is explicitly out of spec scope.

  Headline measurement on a synthetic 320×240 moving-colour-gradient at
  q=50, 15 fps, 5 seconds (75 frames, 1 intra + 74 inter), target
  900 kbps ⇒ target_total 562 500 B: **converged to 562 411 B
  (rel_err = −0.02 %)** with `min_psnr_y = 35.79 dB` across the clip.

- Round 121 tests: `tests/r121_chroma_cbr_convergence.rs` — six tests
  covering (i) the headline 5-second 320×240 chroma CBR convergence at
  900 kbps within ±10 % (release-only — the 75-frame × ≤ 27-trials/frame
  sweep is expensive in debug; `#[cfg(not(debug_assertions))]`-gated),
  (ii) the cap-zero invariant (positive surplus discarded, deficit still
  propagates), (iii) cumulative accumulator equals emitted total
  per-frame, (iv) `reset` / `reset_rate_carry_over` /
  `clear_target_bitrate` semantics on the accumulator, (v) carry-over cap
  clamp semantics + default 8× installation, (vi) the carry-over machinery
  applies identically to the grayscale `encode_intra_gray8` /
  `encode_inter_gray8` path (same `encode_budget_frame` worker).

- Round 113 (grayscale rate control): **target-bitrate / per-frame
  byte-budget mode now applies to the stateful grayscale inter path**
  (`CinepakEncoder::encode_intra_gray8` / `encode_inter_gray8`).

  Round 96 added per-frame byte-budget rate control
  (`with_target_bitrate(bits_per_second, fps)` /
  `with_target_frame_bytes(n)`) to the colour (`Yuv12`) path: the encoder
  sweeps an RD grid around the caller's `EncoderOptions` and commits the
  highest-quality candidate whose encoded size fits a per-frame budget,
  reporting adherence via `last_rate_stats()` and never erroring on
  overshoot. That worker (`encode_budget_frame`) was hardcoded to the
  `Yuv12` pipeline and BT.601 Y-channel SSE scoring, so the round-104
  grayscale stateful inter path explicitly used the caller's `opts`
  verbatim regardless of any configured budget — the documented gap
  "target-bitrate mode is `Yuv12`-scored and does not apply to the
  grayscale path".

  Round 113 threads `PixelMode` through `encode_budget_frame` (and its
  `budget_grid_candidates` / candidate-SSE scorer, now `decode_sse`) and
  routes `encode_intra_gray8` / `encode_inter_gray8` through it when a
  budget is configured. Two mode-specific behaviours:

  - **Grayscale grid drops the no-op `luma_weight` axis.** For `Gray8`
    codebook entries the distance metric scales all four Y dims by
    `luma_weight`, a uniform positive scale that leaves every
    nearest-neighbour / clustering decision invariant (same rationale as
    the round-101 grayscale picker), so the grayscale budget grid is a
    two-axis `(strip_count, rdo_lambda)` cross-product (≤ 9 trials) vs the
    colour grid's three-axis ≤ 27.
  - **Direct-luma SSE scoring.** The 8-bit luminance *is* the Y channel,
    so the candidate ranking uses direct-luma SSE rather than the BT.601
    Y-channel weighting the `Yuv12` path applies to its RGB decode.

  Inter frames stay budgeted and skip / selectively-update against the
  committed previous grayscale reconstruction (the budget sweep rides the
  same snapshot-and-restore carry-over the colour path uses). `reset()`
  preserves the budget; `clear_target_bitrate()` returns the grayscale
  entry points to quality-controlled (verbatim-`opts`) behaviour. Rate
  control is encoder policy, not a bitstream feature — decoded output is
  conformant Cinepak. The colour `encode_intra` / `encode_inter` budget
  path is byte-for-byte unchanged (still three-axis, still BT.601-scored).

- Round 113 tests: `tests/r113_gray_rate_control.rs` — seven tests
  covering (i) generous-budget adherence across an intra+inter grayscale
  sequence (every frame under cap, stays decodable at > 20 dB PSNR),
  (ii) moderate-budget shrink vs the unconstrained grayscale encode at the
  same base quality, (iii) impossible-budget overshoot reporting without
  erroring, (iv) tighter-budget size monotonicity, (v) the inter budget
  riding the stateful carry-over on a static fixture (inter tail collapses
  below the intra keyframe), (vi) the grayscale grid omitting the
  `luma_weight` axis (≤ 9 trials via `RateStats::trials`), (vii)
  `reset` preserving the budget + `clear_target_bitrate` disabling it.

- Round 104 (inter-frame grayscale encode path): **stateful and stateless
  grayscale inter-frame encoders**
  (`CinepakEncoder::encode_intra_gray8` + `encode_inter_gray8` carrying
  rolling grayscale codebooks across frames, plus the stateless
  free-function `encode_gray8_inter` analog of `encode_rgb24_inter`).

  Rounds 2..101 grew the colour (`Rgb24` / 12-bit YUV) encoder a full
  stateful inter pipeline — `CinepakEncoder::encode_intra` /
  `encode_inter` carry a rolling V4/V1 codebook across frames and emit
  `0x3100` SKIP / selective-update / chunk-omission wire patterns — and a
  stateless `encode_rgb24_inter` helper. The grayscale path
  (`encode_gray8` + the round-101 picker) stayed **intra-only**: there
  was no way to carry rolling grayscale codebooks across frames, so a
  grayscale sequence had to re-emit every codebook from scratch every
  frame regardless of how static the content was.

  The per-strip encoder (`encode_intra_strip` / `encode_inter_strip` /
  `encode_inter_strip_with_stats`) was already mode-generic — it takes a
  `PixelMode` and produces the right `0x24xx`/`0x26xx` (grayscale) or
  `0x20xx`/`0x22xx` (12-bit YUV) chunk variants. Round 104 threads
  `PixelMode` through the `CinepakEncoder` intra/inter workers and
  exposes:

  - `CinepakEncoder::encode_intra_gray8(input, w, h, opts)` — Gray8
    intra. Resets rolling state and emits full-replace grayscale
    codebook chunks. The reconstructed frame held internally for the
    next call's SKIP-MB selection is a `Gray8` frame, so subsequent
    `encode_inter_gray8` calls resolve SKIP against the grayscale
    reconstruction.
  - `CinepakEncoder::encode_inter_gray8(input, w, h, opts)` — Gray8
    inter. Carries the rolling grayscale codebook forward with the
    round-5 cross-frame persistence + round-7 stale-slot reclamation
    machinery (mode-generic at the per-strip layer). Returns an error
    if no prior frame has been emitted, or if the prior frame was a
    colour frame (the SKIP comparison and codebook entry layout would
    mismatch).
  - `encode_gray8_inter(input, prev, w, h, opts)` — stateless Gray8
    inter against a Gray8 `prev` reconstruction. Always emits
    full-replace codebook chunks; use the `CinepakEncoder` pair for
    selective-update / chunk-omission wire savings.

  Target-bitrate mode (round 96) is `Yuv12`-scored (BT.601 Y-SSE on the
  RGB picker grid) and does not apply to the grayscale path; the
  grayscale entry points use the caller's `opts` verbatim regardless of
  any configured byte budget.

  Headline win (32×32 four-quadrant grayscale fixture, q=50, intra +
  five identical inter frames):

  | path                                            | inter wire | last-frame SKIPs |
  | ----------------------------------------------- | ---------- | ---------------- |
  | stateless `encode_gray8_inter` (full-replace)   | 2090 B     | 64 / 64          |
  | stateful `encode_inter_gray8` (selective+omit)  | **250 B**  | 64 / 64          |

  **88.0 % wire savings** on the inter-frame tail vs the stateless
  full-replace path, with the last inter frame entirely SKIP — the same
  chunk-omission inheritance win the colour path landed in round 4
  (91.6 % savings on the analogous RGB fixture) now applies to grayscale
  content too.

- Round 104 (decoder fix exposed by grayscale inter): **mode-hint
  fallback in `decode_strip_chunks`**. A grayscale inter frame whose
  strips fully omit their codebook chunks (legal per spec §3.4 of
  `02-codebooks.md` when the inherited codebook + all-SKIP MBs already
  represent the output correctly) used to be misclassified as colour
  because `decode_strip_chunks` defaulted to `Yuv12` when no chunk pinned
  the mode. The decoder now threads `prev_mode_hint: Option<PixelMode>`
  derived from `self.prev_frame.pixel_format` (and from the in-frame
  `frame_mode` once a prior strip has pinned one), and falls back to the
  hint before the `Yuv12` default. A fully chunk-omitted grayscale inter
  frame now correctly stays `Gray8`. Standard frames with codebook
  chunks are unaffected (the in-strip chunk pins the mode and the hint
  is unused).

- Round 104 tests: `tests/r104_gray_inter.rs` — six tests covering
  (i) round-trip of an intra+inter grayscale sequence at ≥ 30 dB PSNR,
  (ii) the **88 % wire-saving headline** vs the stateless path on a
  static fixture with the last inter frame entirely SKIP, (iii) the
  stateless `encode_gray8_inter` decoding cleanly under motion at
  ≥ 28 dB PSNR (observed 45.26 dB on a luma-shifted gradient),
  (iv) `encode_inter_gray8` rejecting calls before any intra, after
  `reset()`, and after a colour intra, (v) the colour stateful path
  unchanged by the mode-threading refactor, (vi) `encode_gray8_inter`
  rejecting a colour `prev` and dimension mismatches.

- Round 101 (grayscale RD-grid picker): **`encode_gray8_best_rd_grid`
  + `encode_gray8_round7`** (Lever N) — the `Gray8` analog of the
  12-bit-YUV frame-level RD-grid picker from rounds 47 / 6 / 7. The
  grayscale encoder previously only emitted a single frame at the
  caller's `opts.strip_count` / `opts.rdo_lambda`; the new picker
  trial-encodes every `(strip_count, rdo_lambda)` combination (default
  `[1, 2, 4]` × `[Some(0.0), Some(2.5), opts.rdo_lambda]`, deduped —
  ≤ 9 trials), self-decodes each, and keeps the bitstream minimising
  direct-luma SSE per pixel plus the Lagrangian byte cost
  (`opts.rdo_lambda · R/N`). On grayscale the 8-bit luminance is the Y
  channel, so distortion is measured directly with no BT.601 weighting;
  there is deliberately no `luma_weight` axis because for `Gray8`
  codebook entries the distance metric scales all four Y dims by
  `luma_weight` (a uniform positive scale that leaves every
  nearest-neighbour / clustering decision invariant). Measured at q=50:
  64×64 gradient **45.01 → 49.56 dB (+4.55 dB, +108 B)**; 320×240
  gradient +5.16 dB (44.84 → 50.00 dB, larger wire); LCG-noise +0.88 dB;
  128×96 gradient unchanged (default already optimal). Intra-only and
  stateless; `rdo_lambda = None` gives pure-distortion ranking. 7 new
  tests in `tests/r101_gray_picker.rs`.

- Round 96 (encoder bitrate-target rate control): **per-frame byte-budget
  mode on `CinepakEncoder`**
  (`CinepakEncoder::with_target_bitrate(bits_per_second, fps)` +
  `with_target_frame_bytes(n)`, in-place `set_*` forms,
  `target_frame_bytes()` / `clear_target_bitrate()` accessors, and the
  `RateStats` telemetry struct queried via `last_rate_stats()`).

  The encoder was ~98 % quality-controlled (the `q`-knob via
  `EncoderOptions::from_quality`). The new mode derives a constant-bitrate
  per-frame budget (`bits_per_second / 8 / fps`, fps clamped to ≥ 1.0,
  fractional rates like 23.976 supported) and drives the existing
  three-axis `(strip_count, rdo_lambda, luma_weight)` RD grid — the same
  3×3×3 deduplicated cross-product `encode_rgb24_round7` sweeps — toward
  that budget per frame, analogous to the ICER `with_byte_budget`
  contract.

  Selection rule (per frame): trial-encode every grid candidate against
  a snapshot of the encoder's carry-over state, then **commit the
  candidate with the lowest BT.601 Y-channel SSE whose encoded size is
  `≤ budget`** (highest-quality fit). When no candidate fits — the budget
  is below the grid's smallest-frame floor — the **smallest** candidate
  is committed and the overshoot is reported via a positive
  `RateStats::byte_delta` with `within_budget == false`. The API never
  errors on rate overshoot.

  Inter frames are budgeted too: each trial decode reuses a clone of the
  pre-frame decoder state so `0x3100` skip / selective-update macroblocks
  resolve against the correct previous reconstruction, and the committed
  candidate's rolling-codebook / prev-frame state is what advances
  `self`. `reset()` now preserves the configured budget (and the
  cross-frame-persistence flag) so a rate-control encoder can be reused
  across independent sequences. `clear_target_bitrate()` returns to
  quality-controlled mode.

  Rate control is an **encoder-policy** choice, not a bitstream feature,
  so the budget-allocation heuristic is derived from first principles per
  the round's docs-gap rule (no external encoder source); decoded output
  remains conformant Cinepak. Measured on a 128×96 gradient (base q=85,
  unconstrained 6418 B): budgets of 6418 / 4813 / 3209 B land at 6148 /
  3952 / 3184 B (95.8 % / 82.1 % / 99.2 % of budget, all under cap);
  budgets below the ~2833 B grid floor emit the 2833 B smallest candidate
  and flag overshoot. Covered by `tests/rate_control_bitrate.rs` (7
  tests: generous-budget adherence across an intra+3-inter sequence,
  moderate-budget shrink vs unconstrained, impossible-budget overshoot
  reporting, size monotonicity across tightening budgets, reset/clear
  semantics, fractional-fps and degenerate-fps arithmetic). `CinepakDecoder`
  and the internal `RollingCodebooks` gained `#[derive(Clone)]` to support
  the trial-and-restore snapshot.

- Round 93 (decoder Sega Saturn deviant variant): **deviant Cinepak
  decode entry point**
  (`CinepakDecoder::decode_deviant_frame` + `DeviantConfig::saturn` /
  `DeviantConfig::lemmings_3do`). Closes the long-standing "Saturn
  deviant codec-frame slicing is deferred until a genuine Saturn
  fixture is acquired" gap in `src/film.rs` (the demuxer recognised
  Saturn `.cpk` headers, but the codec frame body in those files
  failed the standard Cinepak decoder's three structural invariants).

  The deviant variant — documented in
  `docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 125–143
  (Saturn `'cvid'`) and line 189 (Lemmings 3DO 6-byte prefix) —
  diverges from standard Cinepak in three ways:

  1. **Frame-header padding**: 2 extra bytes after the standard
     10-byte frame header (Saturn / Sega CD) or 6 extra bytes
     (Lemmings 3DO), for a 12-byte / 16-byte total prefix before the
     first strip header.
  2. **Short `frame_length`**: the codec header's `frame_length`
     field is 8 bytes shy of the real frame body length. The
     authoritative length comes from the FILM container's `STAB`
     sample-record `sample_length`, not from the codec header.
  3. **Codebook trailing pad**: a `0x2000` codebook chunk may declare
     an 0x5FC-byte payload that contains 255 6-byte vectors + 2
     trailing pad bytes, instead of the standard 0x600-byte payload
     of 256 vectors. The decoder must truncate to
     `floor(payload_len / entry_size)` entries and skip the
     remainder.

  Implementation:
  * `DeviantConfig` struct (`extra_header_bytes`, `frame_length_short_by`,
    `tolerate_codebook_pad`) with `saturn()` / `lemmings_3do()`
    constructors.
  * `CinepakDecoder::decode_deviant_frame(bytes, pts, cfg)` —
    parallels `decode_frame` and shares a private inner that takes
    `Option<DeviantConfig>`. Standard path is unchanged byte-for-byte.
  * `codebook::apply_codebook_chunk_with(kind, payload, cb,
    tolerate_trailing)` — the strict default
    (`apply_codebook_chunk`) still rejects non-divisible payload
    sizes; the new `_with` variant truncates and discards remainder
    bytes when `tolerate_trailing` is `true`.
  * `film::CinepakVariant` enum + `FilmDemuxer::variant()` —
    header-driven classifier that reads `Sega_FILM.wiki`'s decision
    rules (lines 200–224): `'cvid'` + ASCII version ⇒ Saturn
    deviant, `'cvid'` + NULL version ⇒ Lemmings 3DO deviant,
    `'sega'` / `'Seg4'` ⇒ out of scope, anything else ⇒
    out of scope.

  Tests: `tests/deviant_saturn.rs` (8 new tests). Synthesises a
  deviant Cinepak frame exhibiting all three deviations
  simultaneously and verifies (a) the standard decoder rejects it,
  (b) the deviant decoder decodes it to the expected 4×4 Rgb24
  output, (c) Lemmings 3DO config rejects a 2-extra-byte frame and
  vice versa, (d) `FilmDemuxer::variant()` correctly classifies
  Saturn vs Lemmings 3DO vs out-of-scope FILM headers, (e) a
  multi-strip deviant frame decodes coherently with the deviation
  applied per-strip codebook chunk. Plus 2 new codebook-layer unit
  tests (`deviant_full_chunk_tolerates_trailing_pad` /
  `deviant_tolerate_trailing_no_op_when_clean`) covering the
  truncation rule in isolation.

  No external library source consulted at any phase. All wire-format
  facts trace to `docs/video/cinepak/reference/wiki/Sega_FILM.wiki`
  (multimedia.cx mirror, CC-BY-SA) per the
  `docs/IMPLEMENTOR_ROUND.md` clean-room rule.

- Round 9 (encoder-init upgrade — Lever M): **k-means++ initialisation
  for cold-start codebook training**
  (`EncoderOptions::kmeans_pp_init`, default `true`;
  `EncoderOptions::kmeans_pp_lloyd_iter`, default `4`). Rounds 4–8
  fed every cold-start (intra frame / first frame of an inter
  sequence / every strip-picker trial encode) into **median-cut**
  geometric range-bisection followed by LBG + PCL polish. Lever M
  replaces the cold-start with the Arthur–Vassilvitskii 2007
  k-means++ seeding rule (SODA 2007 — published academic algorithm,
  no external library source consulted): sample the first centroid
  uniformly at random, then each subsequent centroid with probability
  proportional to the squared luma-weighted distance from the nearest
  already-chosen centroid; followed by up to 4 Lloyd refinement
  passes (assignment + recentroid + eps-based early stop, same
  semantics as `lloyd_max_iter` / `lloyd_eps`).

  **No-regression guarantee**: the round-9 implementation builds
  **both** the median-cut codebook and the k-means++ codebook and
  keeps whichever has lower total training SSE against the strip's
  vector population. The median-cut output is always a candidate, so
  round 9 can never produce a worse-SSE cold-start than round 8.

  **Determinism**: the sampling RNG is a deterministic xorshift32
  seeded from a content-derived hash (vector population length, first
  / middle / last entry, codebook size, canonicalised luma weight),
  so identical inputs produce byte-identical output. No system
  entropy is consulted — `cargo test` is reproducible across runs.
  `luma_weight = 0` canonicalises to `1` in the seed mix so it stays
  byte-equivalent to `luma_weight = 1` (preserves the round-5
  "0 → 1 fallback" contract).

  **No effect on warm-start**: when a cross-frame seed is available
  (inter strips after the first frame), the encoder continues to use
  the prior codebook centroids and `lloyd_max_iter` Lloyd refinement
  — Lever M triggers only on cold-start paths. Cross-frame slot
  identity is preserved exactly as in round 8.

  Measured deltas (self-encode + self-decode at `q=50`; the picker
  scores Y-SSE per pixel + λ·R/pixel, not pure PSNR):

  | fixture                          | r8 (median-cut) | r9 (k-means++)  | delta            |
  | -------------------------------- | --------------- | --------------- | ---------------- |
  | 64×64 gradient via `round8`      | 44.92 dB/3019B  | 45.10 dB/3037B  | +0.18 dB / +18B  |
  | 320×240 gradient via `round8`    | 46.88 dB/14364B | 46.57 dB/14214B | -0.31 dB / -150B |
  | 64×64 LCG-noise via `round8`     | 23.21 dB/3322B  | 23.29 dB/3322B  | +0.08 dB / +0B   |

  64×64 gradient picker-cost (Y-SSE/N + λ·R/N) drops by 1.1%
  (5.78 → 5.72). On 320×240 the picker shifts its
  `(strip_count, rdo_lambda, luma_weight)` choice to a
  smaller-wire / slightly-lower-PSNR operating point because the
  k-means++ candidate moves the per-trial cost landscape — net wire
  is -150 B at -0.31 dB PSNR_Y, within human-imperceptible bounds on
  a 47 dB gradient.

- Round 9: `tests/r9_psnr.rs` — seven-test validation suite asserting
  (i) round-9 64×64 gradient PSNR_Y doesn't regress by more than
  0.5 dB vs round-8 baseline, (ii) round-9 320×240 gradient PSNR_Y
  doesn't regress by more than 0.5 dB, (iii) strictly positive PSNR_Y
  delta on 64×64 LCG-noise (worst case for VQ), (iv) byte-for-byte
  determinism on identical inputs, (v) `kmeans_pp_init = false`
  reproduces the round-8 median-cut floor (>= 44.5 dB on 64×64
  gradient), (vi) strict picker-cost improvement on 64×64 gradient
  (5.78 → 5.72), (vii) `kmeans_pp_lloyd_iter` monotonicity (more
  iterations don't strictly regress).

- Round 8 (encoder-picker upgrade — Lever L): **per-strip independent
  (lambda, luma_weight) picker** (`encode_rgb24_per_strip_rd` +
  `encode_rgb24_round8` convenience wrapper). Round 7's three-axis
  grid picker (`encode_rgb24_best_rd_grid_3axis` /
  `encode_rgb24_round7`) trial-encodes the **whole frame** with a
  single `(strip_count, rdo_lambda, luma_weight)` per trial; the
  chosen `(rdo_lambda, luma_weight)` pair then applies to every strip
  of the frame. Round 8 plans the strips per `strip_count` candidate
  then for each strip independently sweeps every `(lambda,
  luma_weight)` combination and picks the one minimising per-strip
  Y-SSE + λ·R, assembling the chosen per-strip bitstreams into a
  multi-strip frame.

  Cinepak's bitstream lets each strip carry its own pair of codebooks
  trained independently (spec §3.4 of `02-codebooks.md`), so
  per-strip `luma_weight` and `rdo_lambda` are first-class — the
  decoder doesn't care which lever values the encoder picked per
  strip.

  Each per-strip trial encodes the strip as a standalone single-strip
  frame of size `(width, strip_h)` for scoring, then the winning
  `(lambda, luma_weight)` is re-emitted with the real strip plan
  (correct `y_top` / `y_bottom`) and concatenated into the assembled
  multi-strip frame. Cost: O(|strips| × |lambdas| × |lumas|) trial
  encodes per `strip_count` candidate, plus one final per-strip
  re-encode.

  `encode_rgb24_round8` also runs the round-7 picker and keeps
  whichever pick has the lower Y-SSE + λ·R cost — **guaranteeing
  round 8 ≥ round 7** in the picker's scoring metric. On homogeneous
  content the per-strip greedy converges to the frame-uniform pick
  and round 8 matches round 7 exactly; on heterogeneous content
  round 8 wins.

  Measured wins (self-encode + self-decode at q=50; the picker's
  cost is Y-SSE + λ·R, not pure PSNR — gains show up as either
  smaller wire at iso-quality or higher quality at iso-wire):

  | fixture                                | r7              | r8              | delta            |
  | -------------------------------------- | --------------- | --------------- | ---------------- |
  | 256×256 four-strip-split, default λ=5  | 52.91 dB/10138B | 53.01 dB/9562B  | +0.10 dB / -576B |
  | 320×240 four-strip-split, default λ=5  | 40.27 dB/13209B | 40.27 dB/12669B | +0.00 dB / -540B |
  | 256×256 four-strip-split, λ=None       | 52.93 dB/10570B | 53.01 dB/9562B  | +0.09 dB / -1008B|
  | 64×64 gradient (r7 headline), λ=None   | 45.21 dB/3139B  | 45.21 dB/3139B  | +0.00 dB / +0B   |
  | 320×240 gradient (r7 headline)         | 46.88 dB/14364B | 46.88 dB/14364B | +0.00 dB / +0B   |
  | 64×64 LCG-noise                        | 23.20 dB/3322B  | 23.21 dB/3322B  | +0.01 dB / +0B   |

  Headline: **256×256 four-strip-split now hits 53.01 dB Y at 9562 B
  (vs round-7's 52.91 dB at 10138 B) — a -576 B wire-size reduction
  at +0.10 dB PSNR_Y**, the design win for heterogeneous content. On
  the round-7 64×64 / 320×240 gradient headlines round 8 matches
  round 7 byte-for-byte (per-strip greedy degenerates to the
  frame-uniform pick on homogeneous content).

  Intra-only; the round-8 picker requires a per-strip trial encoder
  and the cross-frame persistence machinery is incompatible with
  switching `luma_weight` mid-sequence.

- Round 8: `tests/r8_psnr.rs` — nine-test validation suite asserting
  (i) round-8 ≤ round-7 in picker cost on 256×256 split content,
  (ii) round-8 ≤ round-7 on 64×64 gradient headline (homogeneous),
  (iii) round-8 ≤ round-7 on 64×64 LCG-noise, (iv) round-8 saves
  ≥ 400 B on 256×256 split-content vs round-7 at default λ=5
  (observed -576 B), (v) round-8 matches round-7 within 0.05 dB
  PSNR_Y on homogeneous content under λ=None, (vi)-(viii) empty-list
  errors on all three candidate lists, (ix) single-candidate
  per-strip picker round-trips at ≥ 30 dB PSNR_Y on the 64×64
  gradient.

- Round 7 (encoder-PCL upgrade — Levers J + K): **three-axis RD grid
  picker with Y-channel scoring**
  (`encode_rgb24_best_rd_grid_3axis` + `encode_rgb24_round7`
  convenience wrapper). Two complementary picker-layer levers on top of
  round 6.

  - **Lever J (`luma_weight` axis)**: the round-6
    `encode_rgb24_best_rd_grid` picker only varies `(strip_count,
    rdo_lambda)` and freezes `luma_weight` at `opts.luma_weight`
    (default `2`). Different fixtures favour different `luma_weight`
    values: the 64×64 gradient at `q=50` likes `luma_weight = 4`
    (+1.14 dB PSNR_Y over `lw=2`), the 320×240 gradient likes
    `luma_weight = 16` at the same wire footprint. The third axis lets
    the picker pivot per-content per-frame instead of requiring the
    caller to guess.

  - **Lever K (Y-channel scoring distortion)**: round 6's picker scores
    by RGB SSE per pixel-channel, but the project's headline quality
    metric is PSNR_Y (BT.601 Y-channel MSE). Higher `luma_weight`
    improves Y at the cost of chroma — RGB-SSE scoring actively
    penalises the `luma_weight` values that boost PSNR_Y the most,
    defeating Lever J. Y-channel scoring aligns the picker's
    optimisation target with the headline metric. (RGB-scoring pickers
    `encode_rgb24_best_strips` / `encode_rgb24_best_rd_grid` are
    unchanged for callers that care about chroma fidelity.)

  Convenience wrapper `encode_rgb24_round7(opts)` sweeps
  `[1, 2, 4]` × `[Some(0.0), Some(2.5), opts.rdo_lambda]` ×
  `[opts.luma_weight, 4, 8]` (deduped) for ≤ 27 trial encodes per
  frame.

  Measured wins (self-encode + self-decode at q=50, PSNR_Y in BT.601 Y
  units):

  | fixture                                                | r6             | r7             | delta              |
  | ------------------------------------------------------ | -------------- | -------------- | ------------------ |
  | 64×64 gradient, `encode_rgb24_round7` headline         | 43.44 dB/2704B | 45.21 dB/3139B | +1.77 dB / +16% B  |
  | 320×240 gradient, `encode_rgb24_round7`                | 45.25 dB/14586B| 46.88 dB/14364B| +1.63 dB / -1.5% B |
  | 64×64 LCG-noise, `encode_rgb24_round7`                 | 23.04 dB       | 23.20 dB       | +0.16 dB           |

  Headline: **the 64×64 gradient via `encode_rgb24_round7` now hits
  45.21 dB Y at 3139 B** — a +7.77 dB lead over ffmpeg's reference
  encoder's ~36.9 dB on the same fixture, +1.77 dB over the round-6
  baseline, and well past the round-7 ≥ 0.5 dB target. On LCG noise
  the lever is a near-wash (random content has no luma-vs-chroma
  structure for the picker to exploit). Both
  `encode_rgb24_best_rd_grid_3axis` and `encode_rgb24_round7` honour
  the existing per-MB knobs (`pcl_max_iter`, `lbg_max_passes`,
  `rdo_lambda` baseline, etc.); the picker only varies the three axes
  declared in the trial grid.

- Round 7: `tests/r7_psnr.rs` — seven-test PSNR validation suite
  asserting (i) `encode_rgb24_round7` on 64×64 gradient breaks
  43.94 dB PSNR_Y (= round-6 baseline + 0.5 dB target; observed
  45.21 dB), (ii) round-7 picker lifts ≥ 0.5 dB over `encode_rgb24_round6`
  on the same fixture (observed +1.77 dB), (iii) Lever-J isolated:
  `luma_candidates = [2, 4]` lifts ≥ 0.8 dB over `[2]`-only (observed
  +1.14 dB), (iv) Lever-K isolated: Y-scoring picker on 320×240
  gradient achieves ≥ r6 PSNR_Y with strictly smaller wire, (v) noisy
  LCG no-regression (≥ 22.5 dB), (vi) empty-list errors on all three
  candidate lists, (vii) wrapper honours `opts.luma_weight` in
  candidate list.

- Round 6 (Levers H + I): **post-classification Lloyd polish (PCL)**
  + **two-axis RD grid picker** at the encoder layer.

  - **Lever H (PCL)**: `EncoderOptions::pcl_max_iter`, default `2`.
    After the round-3 Lagrangian RDO step routes each non-skip MB to
    V4 or V1, each *used* codebook slot is re-trained from only the
    actually-selected member vectors and the per-MB classification is
    re-run. The LBG warm-build (round 4) minimised distortion across
    all non-skip vectors, but the RDO step routes a fraction of them
    to V1 (cheaper wire footprint) — so the LBG centroids aren't the
    means of each slot's actual selected member set. PCL closes that
    gap. Slot identity is preserved (unused slots stay byte-identical
    to the LBG output) so cross-frame persistence and selective-update
    / chunk-omission wins on inter strips are unaffected. 2 iterations
    capture essentially the full gain (the first pass usually
    re-classifies a few V1 MBs to V4 once V4 slots tighten; the second
    is convergence cleanup). Cost per iteration: O(N · K) — one
    nearest-neighbour sweep per non-skip MB times K = codebook size,
    comparable to a single LBG pass.

  - **Lever I (RD grid picker)**: `encode_rgb24_best_rd_grid` +
    `encode_rgb24_round6` convenience wrapper. Trial-encodes every
    `(strip_count, rdo_lambda)` from a user-supplied 2-axis
    cross-product and returns the bitstream minimising
    `R/N + opts.rdo_lambda · D/N` (the same Lagrangian cost as the
    round-3 picker). The round-3 picker only varies `strip_count` and
    reuses `opts.rdo_lambda` for the per-MB RDO of each trial, so it
    can't exploit the V4-saturation regime — on the 64×64 gradient at
    `q=50` with `strip_count=4`, the V4 codebook fills exactly (64
    sub-blocks across 64 entries → exact V4 representation), and
    lowering the per-MB lambda routes more MBs to V4 to harvest the
    residual error. `encode_rgb24_round6` sweeps `[1, 2, 4]` ×
    `[Some(0.0), Some(2.5), opts.rdo_lambda]` for ≤ 9 trial encodes
    per frame.

  Measured wins (self-encode + self-decode at q=50, PSNR_Y in BT.601 Y
  units):

  | fixture                                                | r5             | r6             | delta              |
  | ------------------------------------------------------ | -------------- | -------------- | ------------------ |
  | 64×64 gradient, `encode_rgb24_round6` headline         | 42.39 dB/2554B | 43.44 dB/2704B | +1.05 dB / +5.9% B |
  | 64×64 gradient, `encode_rgb24` strip=1 + PCL only      | 36.55 dB/1066B | 38.19 dB/1132B | +1.64 dB / +6.2% B |
  | 320×240 gradient, `encode_rgb24_best_strips` + PCL     | 41.70 dB/9288B | 43.16 dB/10170B| +1.46 dB / +9.5% B |
  | 64×64 LCG-noise, `encode_rgb24_round6`                 | 23.00 dB       | 23.04 dB       | +0.04 dB           |

  Headline: **the 64×64 gradient via `encode_rgb24_round6` now hits
  43.44 dB Y at 2704 B** — a +6.55 dB lead over ffmpeg's reference
  encoder's ~36.9 dB on the same fixture, +1.05 dB over the round-5
  baseline, and past the round-6 ≥ 0.5 dB target. On pure LCG-noise
  PCL is a near-wash (random content has no luma/spatial structure
  for the post-classification re-training to recover). Set
  `pcl_max_iter = 0` to disable the polish; both
  `encode_rgb24_best_strips` (round-3 picker) and
  `encode_rgb24_best_rd_grid` (round-6 picker) honour it.

- Round 5 (Levers F + G): **luma-weighted distance metric** and
  **luma-prioritized median-cut split** (`EncoderOptions::luma_weight`,
  default `2`). Two complementary luma-priority levers applied at the
  codebook-training layer:

  - **Lever F (distance metric)**: each Y-dim squared-error
    contribution is multiplied by `luma_weight` before being summed
    with the chroma U/V contributions, in `entry_distance` /
    `entry_l1_distance` / `nearest` / `pick_v4` / `pick_v1` and through
    every clustering-related call site (Lloyd refinement, LBG split
    refinement, slot-reclamation residual scoring). Under PSNR_Y,
    packing the codebook tightly in Y is more valuable than packing it
    tightly in U/V — luma-weighted clustering pulls trained centroids
    closer to source Y values at a modest chroma fidelity cost.
  - **Lever G (median-cut split)**: when picking the split dimension
    in `median_cut`, Y-dim extents are multiplied by `luma_weight`
    before being compared against U/V-dim extents. This biases the
    initial bisection toward Y-axis cuts when Y and U/V extents are
    otherwise comparable.

  Both levers share the single `luma_weight` knob (default `2`). `1`
  reproduces the round-4 isotropic-distance / isotropic-split
  behaviour; `0` is treated as `1` internally (no-op fallback).

  Measured wins (self-encode + self-decode at q=50, PSNR_Y in BT.601 Y
  units; `luma_weight = 1` is the round-4 baseline):

  | fixture                                       | r4 (lw=1) | r5 (lw=2) | delta     |
  | --------------------------------------------- | --------- | --------- | --------- |
  | 64×64 gradient, `encode_rgb24`                | 37.85 dB  | 39.39 dB  | +1.55 dB  |
  | 64×64 gradient, `encode_rgb24_best_strips`    | 40.77 dB  | 42.39 dB  | +1.62 dB  |
  | 320×240 gradient, `encode_rgb24_best_strips`  | 40.69 dB  | 41.70 dB  | +1.01 dB  |
  | 64×64 LCG-noise, `encode_rgb24_best_strips`   | 22.98 dB  | 23.00 dB  | +0.02 dB  |

  Headline: **the 64×64 gradient via `encode_rgb24_best_strips` now
  hits 42.39 dB Y at 2554 B** — a +5.49 dB lead over ffmpeg's
  reference encoder's ~36.9 dB on the same fixture, +1.62 dB over the
  round-4 LBG-only baseline, and well past the round-5 41.27 dB
  target. The 320×240 gradient also clears the 41.0 dB target with
  ~0.7 dB of headroom. On pure LCG-noise the lever is a near-wash
  (within ±0.05 dB) — random content has no luma-vs-chroma structure
  for the metric to exploit.

  Set `luma_weight = 1` to disable both Levers F and G and recover
  the round-4 behaviour (used by the regression-guard tests in
  `r5_psnr.rs`, and pinned in the round-4 LBG-isolation tests in
  `r4_psnr.rs` and the round-5/round-6 lever-isolation tests in
  `round5_persistence_and_multistrip.rs` /
  `round6_lloyd_and_window.rs`). Cost: a single integer multiply per
  distance evaluation — no measurable wall-time impact (the
  full-test-suite wall is unchanged).

- Round 5: `tests/r5_psnr.rs` — six-test PSNR validation suite
  asserting (i) `encode_rgb24_best_strips` on 64×64 gradient breaks
  41.27 dB PSNR_Y (observed 42.39 dB), (ii) 320×240 gradient breaks
  41.0 dB (observed 41.70 dB), (iii) LCG-noise no regression vs r4
  (observed +0.02 dB), (iv) Lever F isolated delta ≥ 1.0 dB on 64×64
  gradient (observed +1.55 dB), (v) `luma_weight = 1` reproduces the
  round-4 baseline within noise, (vi) `luma_weight = 0` is a no-op
  fallback (byte-identical to `luma_weight = 1`).

- Round 4 (Lever E): **Linde-Buzo-Gray (LBG) split refinement**
  (`EncoderOptions::lbg_max_passes`, default `8`). After the median-cut
  + Lloyd warm-build builds the strip's V4/V1 codebook, the encoder now
  iteratively identifies the highest-distortion populated slot
  ("splitter") and the lowest-population slot ("donor"), replaces the
  donor with a perturbed copy of the splitter centroid (±1 along the
  splitter cluster's widest dimension), and runs one full Lloyd
  reassignment + recentroid pass. The pass terminates when total SSE
  doesn't strictly decrease — typically within 4..=8 passes on smooth
  content. Reference: Linde, Buzo, Gray (1980) "An Algorithm for Vector
  Quantizer Design", IEEE Trans. Communications 28(1) — published
  VQ-design math, no proprietary source consulted.

  Measured wins (self-encode + self-decode at q=50, PSNR_Y in BT.601 Y
  units; `lbg_max_passes = 0` is the round-3 baseline):

  | fixture                                       | r3 (lbg=0) | r4 (lbg=8) | delta     |
  | --------------------------------------------- | ---------- | ---------- | --------- |
  | 64×64 gradient, `encode_rgb24`                | 36.69 dB   | 37.85 dB   | +1.16 dB  |
  | 64×64 gradient, `encode_rgb24_best_strips`    | 40.23 dB   | 40.77 dB   | +0.53 dB  |
  | 320×240 gradient, `encode_rgb24`              | 35.61 dB   | 37.81 dB   | +2.19 dB  |
  | 320×240 gradient, `encode_rgb24_best_strips`  | 38.17 dB   | 40.69 dB   | +2.52 dB  |
  | 64×64 LCG-noise, `encode_rgb24`               | 21.54 dB   | 22.39 dB   | +0.85 dB  |
  | 64×64 LCG-noise, `encode_rgb24_best_strips`   | 22.09 dB   | 22.98 dB   | +0.89 dB  |

  Headline: **the 64×64 gradient via `encode_rgb24_best_strips` now hits
  40.77 dB Y at 2689 B** — a +3.87 dB lead over ffmpeg's reference
  encoder's ~36.9 dB on the same fixture (measured via
  `tests/ffmpeg_avi_roundtrip.rs`), and well past the 38 dB round-4
  target.

  Set `lbg_max_passes = 0` to disable LBG and recover the round-3
  baseline (used by the regression-guard tests in `r4_psnr.rs`).
  Cost per pass: O(N · K) entry distances (N = vectors, K = codebook
  size) — comparable to a single Lloyd iteration; the pass auto-stops
  when no split improves total SSE, so the bound is informational only
  on well-converged seeds.

- Round 4: `tests/r4_psnr.rs` — five-test PSNR validation suite
  asserting (i) `encode_rgb24_best_strips` on 64×64 gradient breaks
  38 dB PSNR_Y (observed 40.77 dB), (ii) 320×240 gradient stays
  ≥ 38 dB (no regression), (iii) noisy LCG fixture lifts by ≥ 0.5 dB
  vs the round-3 `lbg_max_passes = 0` baseline (observed +0.85 dB),
  (iv) gradient LBG-on vs LBG-off delta ≥ 1.0 dB (observed +1.16 dB),
  (v) `lbg_max_passes = 0` reproduces the round-3 baseline within
  noise.

- Round 3 (round-47): **Lagrangian V1/V4 RDO selection**
  (`EncoderOptions::rdo_lambda`, default `Some(5.0)`). The per-MB
  V1-vs-V4 decision now computes pixel-domain Y SSE for both
  candidate reconstructions and applies the Lagrangian `D + lambda · R`
  rule with a 24-bit rate delta favouring V1 (V4: 4 index bytes; V1:
  1 index byte; flag-bit cost identical). The legacy round-2 path
  compared raw codebook-distance sums (apples-to-oranges: V4 sums 4
  sub-block distances, V1 sums 1, so V1 wins by default on any
  non-pathological gradient and the encoder under-utilised V4's
  per-sub-block fidelity). Measured wins (self-encode + self-decode at
  q=50, PSNR_Y in BT.601 Y units):
  - 320×240 horizontal+vertical gradient: legacy 35.17 dB / 6787 B →
    RDO 35.61 dB / 8548 B (**+0.43 dB at +26% wire**).
  - 64×64 gradient (the `ffmpeg_avi_roundtrip.rs` fixture, ffmpeg's
    own encoder produces ~36.9 dB Y): legacy 35.59 dB / 1438 B → RDO
    36.69 dB / 1573 B (**+1.10 dB at +9% wire**, essentially parity
    with ffmpeg's reference encoder quality on this fixture).

  Set `rdo_lambda = None` to recover the round-2 codebook-distance
  comparison; existing tests that isolate other levers
  (`tests/round5_persistence_and_multistrip.rs::cross_frame_persistence_*`,
  `tests/round6_lloyd_and_window.rs::lloyd_max_iter_0_*`) pin this for
  measurement isolation.

- Round 3 (round-47): **per-frame strip-count picker**
  (`encode_rgb24_best_strips(rgb, w, h, opts, &[u16])`). Trial-encodes
  the input at each candidate strip count and returns the bitstream
  with the lowest Lagrangian cost `R + lambda · D`, where `R` is wire
  size in bytes and `D` is self-decode pixel-domain RGB MSE per pixel.
  Lambda is taken from `opts.rdo_lambda` (falls back to `0.0` when
  `None`, i.e. pick the candidate with the lowest MSE regardless of
  byte cost). On the 320×240 gradient fixture at q=50 the picker
  selects 4 strips (`38.17 dB PSNR_Y / 8568 B`) over the
  `EncoderOptions::from_quality(50)` default of 2 strips (35.61 dB /
  8548 B) — **breaks 38 dB PSNR_Y on a single intra frame** with no
  wire-size increase. Cost: N self-decodes per call (N = number of
  candidates), so use sparingly on a per-frame budget. Stateless;
  intra-only (cross-frame state isn't reset between trials).

- Round 3 (round-47) test: `tests/r3_psnr.rs` — five-test PSNR
  validation suite asserting (i) Lever-D RDO lifts PSNR_Y on both
  benchmark fixtures, (ii) Lever-A strip picker breaks 38 dB on the
  320×240 gradient, (iii) `rdo_lambda = None` legacy path remains
  decodable at the round-2 baseline, (iv) empty-candidates list is
  rejected by `encode_rgb24_best_strips`.

- Round 7: **empty-cluster slot reclamation**
  (`EncoderOptions::stale_slot_threshold`). The stateful
  `CinepakEncoder` now tracks a per-slot staleness counter for each
  V4/V1 codebook flavour: incremented every inter frame the slot is
  not referenced by any macroblock, reset when it is. When a slot's
  counter exceeds `stale_slot_threshold` (default `Some(8)`; `None`
  disables), the encoder reseeds it from the strip's
  highest-residual sample MB (its distance to its nearest seed
  centroid) before Lloyd refinement, then forces a full-replace
  codebook chunk so the decoder sees the reclaimed slot value. Closes
  the long-running cross-frame-persistence "frozen slot" issue where
  an unreferenced slot would survive forever, holding stale content
  the codebook could no longer use. Telemetry exposed via
  `CinepakEncoder::last_frame_stats() -> FrameStats` (reclaimed-slot
  counts per flavour, forced-full-chunk count). Strict-greater-than
  comparison on the threshold means the round-5 8-frame slow-pan
  fixture runs all the way through with persistence active without
  triggering reclamation on the final frame; on a content-cut fixture
  with threshold=2, ~89 slots reclaim across V4+V1 on the cut frame,
  bringing pixel fidelity back to a bounded MAE within 1 frame.
  Per-frame (not per-strip) staleness increments — multi-strip frames
  don't bump the counter once per strip.
- Round 7: **adaptive bisection tolerance**
  (`TwoPassRateControl::encode_at_target_window_bytes_adaptive`).
  Couples the windowed bisection's "are we drifting?" tolerance to
  the running stdev of the prior `window_size` frames' actual byte
  counts: tight on stable scenes (low byte-size variance ⇒ tolerance
  shrinks toward `tolerance_pct_min`), loose on scene changes /
  wipes (high variance ⇒ tolerance grows toward
  `tolerance_pct_max`). `variance_scale_pct` is the stdev-pct above
  which tolerance saturates at the upper bound (default test value
  `25.0` — a common scene-change variance). Equal-bound input
  reproduces the round-6 fixed-tolerance behaviour exactly. First
  two frames use `tolerance_pct_max` (no meaningful stdev yet).

- Round 6: **windowed bisection rate control**
  (`TwoPassRateControl::encode_at_target_window_bytes`). Targets a
  byte budget over a rolling N-frame window rather than per-frame
  (real bitrate-throttled workloads track rolling average, not
  per-frame size). Holds quality steady when the projected window
  sum is within ±tolerance of target; otherwise runs a binary search
  (≤ 8 trials per re-eval) over `q ∈ [0, 100]` for the next frame.
  Composes with round-5's `TwoPassRateControl` (same struct, new
  method) and the existing per-frame
  `encode_at_target_bytes`/`stats_pass`. Empty input returns `Ok([])`;
  starvation-budget paths fall back to `q=0` with positive
  `byte_delta`.
- Round 6: **tighter Lloyd refinement** for the cross-frame codebook
  warm-start (`EncoderOptions::lloyd_max_iter` + `lloyd_eps`). The
  Lloyd loop now iterates up to `lloyd_max_iter` reassignment+update
  passes against the *current* (not seed) centroids, with early stop
  when the largest per-slot Manhattan drift falls to `≤ lloyd_eps`.
  Slot identity is preserved across iterations, so chunk-omission
  / selective-update wins downstream are unaffected. Defaults: 2
  iterations, eps = 1 (round-5 single-pass behaviour preserved by
  setting `lloyd_max_iter = 1`; cold-start by `lloyd_max_iter = 0`).
- Round 6: **ffmpeg-emitted Cinepak-on-AVI roundtrip fixture**
  (`tests/ffmpeg_avi_roundtrip.rs`). Encodes a 64×64 RGB24 frame
  through `ffmpeg -c:v cinepak -f avi -`, parses the resulting AVI's
  `movi/00dc` payload, and decodes it through this crate to assert
  PSNR ≥ 24 dB versus the source. Catches decoder regressions
  against the canonical ffmpeg encoder output without shipping
  binary fixtures. Skips gracefully when ffmpeg is unavailable, the
  cinepak encoder is missing from the local build, or
  `OXIDEAV_SKIP_FFMPEG_TESTS` is set. Observed PSNR locally: 36.9 dB.

- Round 5: **cross-frame codebook persistence at the median-cut
  training step**. `CinepakEncoder::encode_inter` now warm-starts the
  median-cut quantiser with the prior frame's codebook centroids: each
  freshly-sampled vector is assigned to the slot of its nearest seed
  centroid (one Lloyd-style refinement pass), and that slot's new
  centroid is the average of the vectors that landed there. Slots
  with no incoming vectors retain the seed centroid byte-identical,
  which lets the chunk-omission / selective-update path keep firing
  on slow-pan content where most macroblocks shift but the codebook
  population is roughly stable. The seed is also valid across strips
  of the same frame, so multi-strip inter frames inherit the prior
  strip's codebook by default. Toggle via
  `CinepakEncoder::set_cross_frame_codebook_persistence(bool)`
  (default `true`); on a 32×32 slow-pan fixture (8 inter frames at
  q=50) persistence-on shrinks the cumulative inter-frame wire size
  by 5.3% (2807 B vs 2965 B).
- Round 5: **multi-strip inter selective-update verification**. New
  `tests/round5_persistence_and_multistrip.rs::multi_strip_inter_selective_update_beats_full_replace`
  drives a 64×64 4-strip inter encode chain on slow-pan content and
  asserts the stateful selective-update + cross-frame-persistence
  path beats the stateless full-replace path on cumulative wire size
  (observed: **44.0% wire savings** — 4358 B vs 7788 B for 6 inter
  frames). A second test
  (`multi_strip_inter_static_fixture_chunk_omission_across_strips`)
  asserts that on a static 4-strip fixture the inter frame emits
  zero codebook chunks (chunk-omission inheritance across strips).
- Round 5: **two-pass rate control** (`TwoPassRateControl` /
  `RateControlledFrame`). Pass-1 (`stats_pass`) records per-frame
  byte counts at a reference quality; pass-2 (`encode_at_target_bytes`)
  searches an 11-point quality grid for the largest `q` whose
  per-frame byte count is `≤ target_bytes`, falling back to `q=0`
  with a positive `byte_delta` when nothing fits. Per-frame search
  cost is O(N × 11) replays.
- Round 5: **`0x3100` selector-spillover decoder bug fix** + 4 new
  payload-roundtrip regression tests in
  `tests/inter_payload_straddle.rs`. The decoder previously read 1
  index byte for a deferred-V1/V4 placeholder mb in the *current*
  group and never read the rest of its bytes from the *next* group
  (per spec §3.3 step 5 the index bytes belong wholly to the next
  group). The fix defers the placeholder push until step 1 of the
  next iteration so its bytes are read with the correct group;
  static-fixture / mixed-V1+V4-+Skip patterns that triggered this
  now round-trip cleanly.

- Round 4: **selective-update codebook chunks on inter strips**
  (`CinepakEncoder` stateful struct). The encoder tracks the rolling
  V4/V1 codebook the decoder will hold across strips and frames, and
  for each inter strip emits a `0x2100` / `0x2300` selective-update
  chunk (only changed slots) when smaller than the equivalent
  `0x2000` / `0x2200` full-replace — or omits the codebook chunk
  entirely (spec §3.4: "header-only / omitted = inherit previous
  strip's codebook") when the previous codebook already has the
  correct values for every slot the strip's macroblocks reference.
  Free-function entry points (`encode_rgb24` / `encode_rgb24_inter` /
  `encode_gray8`) are unchanged — they remain stateless and always
  emit full-replace. Public surface: `CinepakEncoder::new` /
  `::reset` / `::encode_intra` / `::encode_inter`.
- Round 4: **multi-frame static-fixture wire-size validation test**
  (`tests/static_fixture_multi_frame.rs`). Drives `CinepakEncoder`
  with one intra keyframe + five identical inter frames and asserts:
  (a) every frame round-trips through `CinepakDecoder` within the
  codec's quantisation tolerance, (b) total inter-frame wire size is
  strictly smaller than the equivalent stateless `encode_rgb24_inter`
  run (observed: 91.6% wire savings — 250 B vs 2990 B for 5 inter
  frames at quality 50), (c) the SKIP-MB fraction grows / saturates
  across frames (observed: 64 / 64 macroblocks SKIP'd from frame 1
  through frame 5 once the rolling codebook stabilises).

### Round 3

- Round 3: **multi-strip encoder** (`EncoderOptions::strip_count`).
  Splits the frame into N horizontal bands and runs the median-cut
  quantiser independently per strip, so each band gets its own V1+V4
  codebook pair tuned to its local pixel population. Round-trips
  through the decoder with the y-sentinel rule (first strip carries
  absolute coords; subsequent strips ride on the previous strip's
  `y_bottom`).
- Round 3: **inter-frame encoder** (`encode_rgb24_inter`). For each
  macroblock, computes per-pixel MSE against the same-position 4×4
  block in the previous reconstructed frame; below
  `EncoderOptions::skip_threshold` the macroblock is emitted as a
  `0x3100` SKIP code, otherwise a V1/V4 update is selected by the
  same nearest-neighbour rule as intra. Strips are tagged
  `STRIP_ID_INTER` (`0x1100`) and the vector chunk is `0x3100`.
- Round 3: **PSNR-driven quality knob**
  (`EncoderOptions::from_quality(q)`, `q ∈ 0..=100`). Maps to codebook
  size (8..=256, log scale), strip count (1..=4), and skip threshold
  (256.0..=16.0, exponential). One call covers the full quality–wire-size
  trade-off curve without hand-tuning each knob.
- Round 3: **black-box behavioural verification against FFmpeg**
  (`tests/ffmpeg_psnr.rs`). Encodes a synthetic 320×240 RGB24 gradient
  fixture with `quality = 50`, packages the frame in a minimal AVI
  container, decodes via system `ffmpeg` (treated as an opaque CLI
  oracle), and asserts PSNR ≥ 28 dB versus the source. Observed PSNR
  ≈ 30.1 dB. Test gracefully skips when `ffmpeg` is unavailable or
  `OXIDEAV_SKIP_FFMPEG_TESTS` is set.
- Round 2: synth-test coverage for the four selective-update codebook
  chunk types FFmpeg never emits (`0x2100` V4 YUV update, `0x2500` V4
  grayscale update, `0x2700` V1 grayscale update; the existing
  `0x2300` already had a regression test). Spec §4.4 documents these
  as "wire-legal but encoder-optional"; the corresponding decode path
  was already in round 1 but lacked dedicated test coverage.
- Round 2: median-cut codebook-quantiser **encoder** (`encode_rgb24` /
  `encode_gray8`). Single-strip intra only, configurable per-codebook
  entry count (default 64+64, matching FFmpeg `-q:v 10`). Self-roundtrip
  tests confirm pixel-correctness within codec quantisation tolerance
  (±10 on RGB, ±8 on luma).
- Round 2: **Sega FILM (CPK) demuxer** (`FilmDemuxer::parse`,
  `probe_film`). Parses the FILM header + FDSC + STAB chunks per spec
  §2 of `05-container-carriage.md`. Supports both the standard 32-byte
  FDSC layout (FFmpeg `film_cpk` muxer) and the abbreviated 20-byte
  layout (early Sega CD variants). Sample-record helpers
  (`is_keyframe`, `is_audio`, `timestamp_ticks`) decode the
  `sample_info_1` discriminator per §2.4.
- Round 2: **probe function** for the `CVID` FourCC registration.
  Validates the 10-byte frame header + 12-byte first-strip header
  structure (flags, width/height alignment, strip_id 0x1000/0x1100,
  size accounting). Returns 1.0 on structural validity, 0.5 without
  packet bytes, 0.0 on failed checks.
- Round 2: `encode_selective_chunk` helper for synthesising
  `0x2100`/`0x2300`/`0x2500`/`0x2700` chunks; used by the
  selective-chunk encode-roundtrip test.

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
