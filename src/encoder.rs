//! Cinepak frame-level encoder.
//!
//! Produces conformant Cinepak bitstreams (round 3: multi-strip, intra
//! and inter, mixed V1+V4 codebooks, optional skip macroblocks for
//! inter) that round-trip through this crate's decoder. The encoder is
//! **reference-grade**: it does not aim to match FFmpeg's per-byte
//! output or its rate-control behaviour, only to emit
//! syntactically-valid Cinepak frames whose decoded pixels recover the
//! input within codebook quantisation error.
//!
//! ## Algorithm overview
//!
//! Encoding proceeds per macroblock (4×4 RGB pixel block):
//!
//! 1. Convert the input RGB block to the codec's `(Y0..Y3, U, V)`
//!    representation using the **forward** of the spec's inverse
//!    matrix from `04-yuv-rgb-matrix.md`.
//! 2. Build candidate **V1** and **V4** codebooks via a median-cut
//!    quantiser over the per-macroblock vectors **of each strip**
//!    (one V1 vector per MB, four V4 sub-block vectors per MB).
//! 3. Choose **V1** vs **V4** per macroblock by lower mean-squared
//!    error against the original block, breaking ties toward V1
//!    (smaller wire footprint).
//! 4. For **inter** strips: when an MB matches the previous-frame
//!    reconstructed pixels within an MSE threshold, emit it as a
//!    SKIP code (0x3100 vector chunk grammar `0`). Otherwise emit
//!    the chosen V1/V4 update.
//! 5. Emit the strip with `0x2000` (V4 full) + `0x2200` (V1 full)
//!    codebook chunks (or `0x2400`/`0x2600` for grayscale) and a
//!    `0x3000` mixed-intra (or `0x3100` mixed-inter) vector chunk.
//!
//! Multi-strip frames split the image into N horizontal bands, each
//! coded independently with its own codebook pair. Strip count is
//! derived from `quality` (more strips = better local adaptation =
//! higher PSNR, at the cost of larger wire size from N codebook
//! chunks).
//!
//! ## Selective-update codebook chunks (round 4)
//!
//! For **inter** strips emitted via [`CinepakEncoder::encode_inter`],
//! the encoder tracks the rolling V4/V1 codebook state that the
//! decoder will see (across strips and across frames) and, for each
//! strip, compares the freshly-trained codebook against the previous
//! one. If most slots are unchanged it emits a `0x2100` / `0x2300`
//! selective-update chunk (only changed slots) instead of `0x2000` /
//! `0x2200` full-replace; on a perfectly-static fixture the codebook
//! is byte-identical and no codebook chunk is emitted at all (spec
//! §3.4: omitted codebook chunk = inherit previous strip's).
//!
//! Wire-size cost (per chunk):
//!
//! ```text
//! full-replace V4 12-bit YUV      = 4 + N × 6           (N = entry count)
//! selective-update V4 12-bit YUV  = 4 + Σ_g (4 + 6 × popcount(flag_g))
//! ```
//!
//! Selective wins when the changed-slot count `K` plus the group-flag
//! overhead beats `N × 6 / entry_size`. The encoder picks whichever is
//! smaller, falling back to full-replace on first frames or whenever
//! `K ≥ N`.
//!
//! Free-function entry points ([`encode_rgb24`] / [`encode_rgb24_inter`]
//! / [`encode_gray8`]) remain stateless and always emit full-replace —
//! they don't carry the prior-codebook context selective updates need.
//!
//! ## Limitations
//!
//! - 12-bit YUV via `encode_rgb24` / `encode_rgb24_inter` /
//!   [`CinepakEncoder`]; 8-bit grayscale via `encode_gray8` only.
//! - Selective-update chunks are emitted by the stateful
//!   [`CinepakEncoder`] only.

// The encoder uses index-based loops (`for sub_idx in 0..4`) to keep
// the spatial-position arithmetic (`sub_row = idx / 2`, `sub_col = idx
// % 2`) inline with the index — switching to enumerate-based form
// hides that derivation behind a tuple.
#![allow(clippy::needless_range_loop)]

use crate::codebook::{
    encode_full_chunk, encode_selective_chunk, Codebook, CodebookChunkKind, CodebookEntry,
    PixelMode, UpdateStyle, WhichCodebook, CHUNK_HEADER_SIZE,
};
use crate::error::{CinepakError, Result};
use crate::header::{
    FrameHeader, RawStripHeader, FRAME_HEADER_SIZE, STRIP_HEADER_SIZE, STRIP_ID_INTER,
    STRIP_ID_INTRA,
};
use crate::image::{CinepakFrame, CinepakPixelFormat};
use crate::vector::{
    encode_inter_payload, encode_mixed_intra_payload, Mb, VECTOR_CHUNK_INTER, VECTOR_CHUNK_INTRA,
};

/// Encoder configuration.
///
/// Three knobs at most are touched directly:
///
/// - `v4_entries` / `v1_entries` — codebook size per strip
///   (1..=256 each).
/// - `strip_count` — number of horizontal strips per frame
///   (1..=number of macroblock rows).
/// - `skip_threshold` — MSE-per-pixel threshold below which an inter
///   macroblock is coded as SKIP. Lower = more updates = larger frame
///   but better quality; higher = more skips = smaller / lower quality.
///
/// For most users [`EncoderOptions::from_quality`] is sufficient: it
/// derives all three from a single `quality ∈ 0..=100` PSNR-style
/// knob (see method docs for the mapping).
#[derive(Clone, Copy, Debug)]
pub struct EncoderOptions {
    /// Number of V4 codebook entries (1..=256). Default `64`.
    pub v4_entries: u16,
    /// Number of V1 codebook entries (1..=256). Default `64`.
    pub v1_entries: u16,
    /// Number of horizontal strips per frame (1..=mb_rows). Default
    /// `1`. The encoder clamps this down to the number of macroblock
    /// rows when the frame is too short to host that many strips.
    pub strip_count: u16,
    /// Per-pixel MSE threshold below which an inter macroblock is
    /// coded as SKIP. Default `64.0` (≈ ±8 per channel).
    pub skip_threshold: f32,
    /// Round-6 tighter Lloyd refinement: maximum number of
    /// Lloyd-style refinement iterations applied to the seeded
    /// median-cut warm-start (cross-frame codebook persistence). The
    /// quantiser stops earlier if the per-iteration centroid movement
    /// drops below [`Self::lloyd_eps`]. `1` reproduces the round-5
    /// single-pass behaviour; `0` disables refinement entirely (raw
    /// nearest-seed assignment with no centroid update). Default `2`.
    pub lloyd_max_iter: u8,
    /// Round-6 tighter Lloyd refinement: early-stop epsilon. When the
    /// largest per-slot centroid Manhattan distance from the previous
    /// iteration is `≤ lloyd_eps`, refinement halts before reaching
    /// `lloyd_max_iter`. Measured in raw codebook-entry units summed
    /// across all 4..=6 dims (L1 norm). Default `1` (one unit of
    /// total drift across all dims = "stable enough"). Only takes
    /// effect when `lloyd_max_iter ≥ 2`.
    pub lloyd_eps: u32,
    /// Round-7 empty-cluster slot reclamation: number of consecutive
    /// inter frames a Lloyd slot may go *unreferenced* (no MB in this
    /// strip's codebook flavour points at it) before the encoder
    /// recycles it for a high-residual MB instead of letting it sit
    /// frozen forever via cross-frame persistence.
    ///
    /// `None` disables reclamation (round-6 behaviour: a slot that
    /// becomes unreferenced stays byte-identical across frames as long
    /// as the chunk-omission / selective-update path can avoid
    /// emitting it). `Some(n)` reclaims any slot that has been
    /// unreferenced for **strictly more than** `n` consecutive inter
    /// frames *during this encoder session*; the encoder reseeds those
    /// slots from the strip's high-residual macroblocks (those whose
    /// nearest-codebook error is largest) and **forces a full-replace
    /// chunk emission** for the codebook flavour whose slot was
    /// reclaimed, since the decoder needs to see the new slot value
    /// (selective-update + the reclaimed slot list also works, but
    /// full-replace is simpler and the wire-size delta is small for a
    /// one-shot reclaim). Strict-greater-than rather than ≥ so an
    /// 8-frame slow-pan can run all the way through with persistence
    /// active without triggering reclamation on the final frame.
    ///
    /// Default `Some(8)`: a slot frozen for 8 consecutive inter frames
    /// is recycled. On slow-pan content this typically never triggers;
    /// on scene changes / wipes it triggers within a few frames and
    /// adapts the codebook to the new content faster than waiting for
    /// median-cut to overwhelm the inertia.
    pub stale_slot_threshold: Option<u8>,
    /// Round-3 (round-47): **Lagrangian V1/V4 rate-distortion
    /// selection**. When `Some(lambda)`, the per-MB V1-vs-V4 decision
    /// computes pixel-domain Y SSE for both reconstructions and picks
    /// the one minimising `D + lambda · R`, where `R` is the bit cost
    /// (V1 costs ~10 bits per inter MB / 9 bits intra; V4 costs ~34 /
    /// 33 — V4 has 24 more bits per MB than V1).
    ///
    /// `None` reproduces the legacy round-2 behaviour (compare the
    /// raw codebook-distance sums; tiebreak toward V1) — kept for
    /// regression / A-B testing.
    ///
    /// Lambda interpretation: pixel-domain Y SSE per bit. A lambda of
    /// `0.0` ignores rate entirely (always pick the better-quality
    /// reconstruction = V4 essentially always); a very large lambda
    /// always picks V1. A value around `5.0` for typical natural
    /// content yields a meaningful PSNR_Y win (+1..+2 dB on smooth
    /// gradients) at a modest wire-size cost.
    ///
    /// Default `Some(5.0)`. Set to `None` to recover the round-2
    /// "lowest codebook distance wins, tiebreak V1" behaviour.
    pub rdo_lambda: Option<f32>,
    /// Round 4 (Lever E): **Linde-Buzo-Gray (LBG) split refinement
    /// passes** applied to each strip's V4 and V1 codebook after the
    /// initial median-cut + Lloyd build (Linde, Buzo, Gray 1980 — "An
    /// Algorithm for Vector Quantizer Design", IEEE Trans. Communications
    /// 28(1)). Each pass identifies the highest-distortion populated
    /// codebook slot and "donates" the lowest-population slot to host a
    /// perturbed split of the high-distortion centroid, then runs one
    /// extra Lloyd reassignment+recentroid pass over **all** vectors. The
    /// pass terminates early if no split improves total SSE.
    ///
    /// Tuning: even `1..=2` passes lift PSNR_Y by ~1.5 dB on the smooth
    /// gradient fixtures (the high-population, mid-luma codebook slots
    /// of the median-cut output have outsized residuals that a single
    /// split-and-Lloyd can absorb); `0` disables LBG. Beyond 4 passes
    /// the returns diminish to noise. Cost per pass: O(N · K) entry
    /// distances, where N = vectors, K = codebook size — comparable to a
    /// single Lloyd iteration.
    ///
    /// Default `8` (within-noise of optimal on natural content).
    pub lbg_max_passes: u8,
    /// Round 5 (Lever F): **luma-weighted distance metric** used for
    /// codebook training (clustering / Lloyd reassignment / LBG split
    /// refinement) and per-MB nearest-neighbour selection. Each Y-dim
    /// (`Y0..Y3`) squared-error contribution is multiplied by
    /// `luma_weight` before being summed with the chroma `U/V`
    /// contributions. PSNR_Y measures only the luma channel, so weighting
    /// luma above chroma pulls the trained codebook closer to the source
    /// Y values at a modest fidelity cost on chroma. Also scales each
    /// Y-dim extent by `luma_weight` in `median_cut`'s split-dimension
    /// selection (Lever G — see below), so the initial bisection
    /// prefers Y-axis cuts when Y and U/V extents are otherwise
    /// comparable.
    ///
    /// `1` reproduces the round-4 isotropic distance (Y and U/V weighted
    /// equally). Higher values bias more aggressively toward luma fidelity:
    /// `2` is the round-5 default (modest +0.5..+1.0 dB PSNR_Y on smooth
    /// gradients, small chroma-PSNR cost); `4` extracts another fraction
    /// of a dB but starts to visibly desaturate chroma transitions on
    /// natural content. `0` falls back to `1` internally (no luma weight).
    ///
    /// In `Gray8` mode the parameter is a no-op (there are no chroma
    /// dims to weight against). Default `2`.
    pub luma_weight: u8,
    /// Round 6 (Lever H): **post-classification Lloyd polish** — number
    /// of iterations of "reclassify MBs → recompute used-slot centroids
    /// from the actually-selected vectors → reclassify again". Each
    /// iteration:
    ///
    /// 1. Walks the per-MB classification produced by the RDO step
    ///    (`Mb::V4(idx)` or `Mb::V1(idx)` per non-skip MB).
    /// 2. For each *used* V4 slot, re-averages the codebook entry from
    ///    only the sub-block vectors that landed on it (rather than the
    ///    full vector population the LBG pass trained against, which
    ///    includes vectors that the RDO step then routed to V1).
    /// 3. Same for each used V1 slot.
    /// 4. Re-runs the per-MB V4/V1 RDO selection against the polished
    ///    codebook.
    ///
    /// **Why this helps**: the LBG-refined codebook minimises total
    /// distortion across **all** non-skip vectors, but the RDO step then
    /// routes a substantial fraction of them to V1 (cheaper) rather than
    /// V4. Polishing each codebook entry against the *actually-selected*
    /// subset gives a tighter codebook for the wire-footprint the RDO
    /// chose, without changing the wire structure (slot identity is
    /// preserved). Unused slots stay byte-identical to the LBG output —
    /// so cross-frame persistence and selective-update / chunk-omission
    /// wins on inter strips are unaffected.
    ///
    /// **Cost per iteration**: O(N · K) — one nearest-neighbour sweep
    /// per non-skip MB times K = codebook size — comparable to a single
    /// LBG pass.
    ///
    /// `0` disables the polish (round-5 behaviour). `2` is the round-6
    /// default; `1` captures most of the gain (the second iteration
    /// further refines the codebook against the *re-classified* MB
    /// assignments, which converges quickly).
    ///
    /// Default `2`.
    pub pcl_max_iter: u8,
    /// Round 9 (Lever M): **k-means++ initialisation** for cold-start
    /// codebook training. When `true` and no cross-frame seed is
    /// available for the strip (intra frame / first frame / `None`
    /// seed), the warm-build replaces median-cut's geometric
    /// range-bisection with the k-means++ seeding rule (Arthur &
    /// Vassilvitskii 2007, SODA): pick the first centroid uniformly at
    /// random from the vector population, then for each subsequent
    /// centroid sample from the remaining vectors with probability
    /// proportional to the squared luma-weighted distance from the
    /// nearest already-chosen centroid. After the K centroids are
    /// chosen, [`Self::kmeans_pp_lloyd_iter`] Lloyd refinement passes
    /// (assignment + recentroid) polish them.
    ///
    /// **Why this can win**: median-cut's bisection is greedy on the
    /// per-cluster widest dimension at the time of the split, so a
    /// pair of dense clusters along the *same* dim can end up sharing
    /// a centroid while a sparser dim absorbs more centroids than it
    /// needs. k-means++ samples each new centroid from the actual
    /// residual-distance distribution of the data, which provably
    /// achieves an O(log K)-approximation to the optimal total-SSE
    /// initialisation in expectation. On Cinepak content, the
    /// 2×2-sub-block vectors are heavily clustered around mid-luma /
    /// mid-chroma points, and the long tail of high-luma / saturated
    /// outliers needs proportional centroid coverage rather than the
    /// median-cut's "everything between the highest two values gets
    /// one centroid" rule.
    ///
    /// **Determinism**: the sampling RNG is a xorshift32 seeded
    /// deterministically from the vector population's content
    /// (`hash(len, first, last, mid)` mixed with the requested codebook
    /// size + luma weight). Identical inputs ⇒ identical codebooks ⇒
    /// identical wire bytes ⇒ identical decode. No system entropy is
    /// ever consulted.
    ///
    /// **No effect on seeded path**: when a cross-frame seed is
    /// available (inter strips after the first frame), the warm-start
    /// continues to use the prior codebook centroids — k-means++
    /// triggers only on cold-start cases. Slot-identity preservation
    /// across frames is therefore unaffected.
    ///
    /// `false` reproduces the round-8 median-cut cold-start behaviour
    /// exactly. Default `true`.
    pub kmeans_pp_init: bool,
    /// Round 9 (Lever M): number of Lloyd refinement passes applied
    /// after the k-means++ seed selection (see
    /// [`Self::kmeans_pp_init`]). Same semantics as
    /// [`Self::lloyd_max_iter`]: each pass reassigns vectors to the
    /// nearest current centroid then recomputes each centroid as the
    /// mean of its assigned cluster, with early stop when the largest
    /// per-slot Manhattan drift falls to `≤ lloyd_eps`. `0` keeps the
    /// raw k-means++ seed without any Lloyd polish (rarely useful);
    /// `1..=4` captures most of the gain in practice. The pass cap is
    /// independent of `lloyd_max_iter` because the cold-start path
    /// benefits from a few more iterations than the warm-start path
    /// (the warm-start centroids are already close to the optimum;
    /// k-means++ seeds may need 2..=4 reassignments to converge).
    ///
    /// Default `4`. Ignored when `kmeans_pp_init = false` or when the
    /// strip has a cross-frame seed.
    pub kmeans_pp_lloyd_iter: u8,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        Self {
            v4_entries: 64,
            v1_entries: 64,
            strip_count: 1,
            skip_threshold: 64.0,
            lloyd_max_iter: 2,
            lloyd_eps: 1,
            stale_slot_threshold: Some(8),
            // Round-3 (round-47) Lagrangian V1/V4 selection: lambda=5.0
            // tuned on the 320×240 gradient fixture (+~2 dB PSNR_Y).
            rdo_lambda: Some(5.0),
            // Round 4 default: 8 LBG split-refinement passes after the
            // median-cut + Lloyd warm-build. Within-noise of the
            // unbounded-passes optimum on smooth-gradient and
            // textured-noise fixtures alike.
            lbg_max_passes: 8,
            // Round 5 default: luma-weighted distance with Y dims at 2×
            // the weight of U/V dims during codebook training.
            luma_weight: 2,
            // Round 6 default (Lever H): 2 iterations of post-classification
            // Lloyd polish after the LBG warm-build + RDO classification.
            pcl_max_iter: 2,
            // Round 9 (Lever M): k-means++ cold-start initialisation
            // with 4 Lloyd refinement passes.
            kmeans_pp_init: true,
            kmeans_pp_lloyd_iter: 4,
        }
    }
}

impl EncoderOptions {
    /// Map a `quality ∈ 0..=100` PSNR-style knob to a full options
    /// vector.
    ///
    /// Mapping (clamped at the endpoints):
    ///
    /// | quality | v4_entries | v1_entries | strip_count | skip_thr |
    /// | ------- | ---------- | ---------- | ----------- | -------- |
    /// |       0 |          8 |          8 |           1 |    256.0 |
    /// |      25 |         32 |         32 |           1 |    128.0 |
    /// |      50 |         64 |         64 |           2 |     64.0 |
    /// |      75 |        128 |        128 |           3 |     32.0 |
    /// |     100 |        256 |        256 |           4 |     16.0 |
    ///
    /// Higher quality ⇒ larger codebooks (more colour fidelity per
    /// strip), more strips (better local adaptation, especially for
    /// images with vertical colour gradients), and a stricter SKIP
    /// threshold (more updates emitted on inter frames, less ghosting).
    ///
    /// The mapping is intentionally coarse: Cinepak's quality is
    /// dominated by the 4×4-block structure and 12-bit YUV chroma
    /// quantisation, so doubling the codebook size only produces ~3 dB
    /// of PSNR gain on natural content.
    pub fn from_quality(quality: u8) -> Self {
        let q = quality.min(100) as f32;
        // Logarithmic-ish ramp on codebook size: 8..=256.
        // 2^(3 + 5*q/100) ⇒ q=0 → 8, q=100 → 256.
        let n = (2.0f32.powf(3.0 + 5.0 * q / 100.0)).round() as u16;
        let n = n.clamp(1, 256);
        // Strip count: 1..=4 across the q range, with thresholds at
        // q=33 / 66 / 100. We don't go higher than 4 because each
        // extra strip costs a full codebook chunk.
        let strip_count = if q < 33.0 {
            1
        } else if q < 66.0 {
            2
        } else if q < 100.0 {
            3
        } else {
            4
        };
        // Skip threshold: 256.0 at q=0 → 16.0 at q=100, exponential.
        let skip_threshold = 256.0 * 2.0f32.powf(-q / 25.0);
        Self {
            v4_entries: n,
            v1_entries: n,
            strip_count,
            skip_threshold,
            // Round-6 default: 2 Lloyd iterations with eps=1 (early-stop
            // on essentially-stable centroids).
            lloyd_max_iter: 2,
            lloyd_eps: 1,
            // Round-7 default: reclaim slots frozen for 8 consecutive
            // inter frames.
            stale_slot_threshold: Some(8),
            // Round-3 (round-47) Lagrangian V1/V4 RDO selection.
            rdo_lambda: Some(5.0),
            // Round 4 default: 8 LBG split-refinement passes (Lever E).
            // Within-noise of the unbounded-passes optimum on
            // smooth-gradient and textured-noise fixtures alike.
            lbg_max_passes: 8,
            // Round 5 default (Lever F): luma weight 2 — Y squared-error
            // contributions count twice as much as U/V in the distance
            // metric used for clustering and nearest-neighbour selection.
            luma_weight: 2,
            // Round 6 default (Lever H): 2 iterations of post-classification
            // Lloyd polish.
            pcl_max_iter: 2,
            // Round 9 (Lever M): k-means++ cold-start initialisation
            // with 4 Lloyd refinement passes.
            kmeans_pp_init: true,
            kmeans_pp_lloyd_iter: 4,
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Encode a 12-bit YUV intra frame from packed `Rgb24` input
/// (`width × height × 3` bytes, R, G, B in row-major scan).
pub fn encode_rgb24(rgb: &[u8], width: u32, height: u32, opts: EncoderOptions) -> Result<Vec<u8>> {
    encode_intra_frame(rgb, width, height, PixelMode::Yuv12, opts)
}

/// Round-3 (round-47) Lever A: **per-frame strip-count picker** for
/// intra frames. Trial-encodes the input at each of the supplied
/// `candidates` strip counts and returns the bitstream with the lowest
/// Lagrangian cost `R + lambda · D`, where `R` is the wire-size in
/// bytes and `D` is the self-decode pixel-domain RGB SSE divided by
/// total pixel count (so `D` has units of "MSE per pixel" — typically
/// 1..=1000 on natural content). `lambda` is taken from
/// `opts.rdo_lambda`; if `None`, falls back to the pure-distortion
/// minimum (`R · 0 + D`) — i.e., picks the candidate with the lowest
/// MSE regardless of byte cost.
///
/// `candidates` must be non-empty and each entry in
/// `1..=(height / 4)`; any value outside that range is silently
/// clamped by [`plan_strips`].
///
/// The picker is useful when the optimal strip count varies with
/// frame content (e.g., uniform colour favours 1 strip; vertical
/// gradients favour 4+ strips). On a 320×240 horizontal+vertical
/// gradient at `q=50` with `rdo_lambda = Some(5.0)`, the picker
/// reliably selects 4 strips (38.2 dB PSNR_Y) over the default 2 strips
/// (35.6 dB).
///
/// Returns the encoded bitstream of the winning candidate. The chosen
/// strip count is observable via the bytestream's frame-header
/// `strip_count` field (parsed with `FrameHeader::parse`).
pub fn encode_rgb24_best_strips(
    rgb: &[u8],
    width: u32,
    height: u32,
    opts: EncoderOptions,
    candidates: &[u16],
) -> Result<Vec<u8>> {
    if candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_best_strips: candidates list must not be empty",
        ));
    }
    let lambda = opts.rdo_lambda.unwrap_or(0.0) as f64;
    let n_pixels = (width as usize) * (height as usize);
    let mut best: Option<(f64, Vec<u8>)> = None;
    for &strip_count in candidates {
        let trial_opts = EncoderOptions {
            strip_count,
            ..opts
        };
        let bytes = encode_rgb24(rgb, width, height, trial_opts)?;
        // Self-decode and compute pixel-domain RGB MSE.
        let mut dec = crate::decoder::CinepakDecoder::new();
        let frame = dec.decode_frame(&bytes, None)?;
        let stride = frame.stride();
        let pixels = frame.pixels();
        let mut sum_sq: f64 = 0.0;
        for r in 0..height as usize {
            let off_dec = r * stride;
            let off_src = r * (width as usize) * 3;
            for c in 0..(width as usize) * 3 {
                let d = pixels[off_dec + c] as f64 - rgb[off_src + c] as f64;
                sum_sq += d * d;
            }
        }
        let d_per_pixel = sum_sq / (n_pixels as f64 * 3.0);
        let r_bytes = bytes.len() as f64;
        let cost = d_per_pixel + lambda * r_bytes / n_pixels as f64;
        // Lower cost wins; tiebreak toward smaller wire size.
        let better = match &best {
            None => true,
            Some((best_cost, best_bytes)) => {
                cost < *best_cost || (cost == *best_cost && r_bytes < best_bytes.len() as f64)
            }
        };
        if better {
            best = Some((cost, bytes));
        }
    }
    Ok(best.expect("at least one candidate processed").1)
}

/// Round-6 (Lever I) two-axis RD grid picker: trial-encode the input
/// at every `(strip_count, rdo_lambda)` combination from the
/// `strip_candidates` × `lambda_candidates` cross-product and return the
/// bitstream minimising the same Lagrangian cost as
/// [`encode_rgb24_best_strips`] (`R/N + lambda · D/N`, scoring lambda
/// taken from `opts.rdo_lambda`).
///
/// Why this beats [`encode_rgb24_best_strips`]: the round-3 picker only
/// varies `strip_count` and uses a fixed per-MB RDO lambda from
/// `opts.rdo_lambda`. But on small frames where the V4 codebook is
/// effectively saturated (e.g. `64×64` at `q=50` with `strip_count=4`
/// gives `64` V4 sub-blocks across `64` codebook slots — exact V4
/// representation), the dominant residual error comes from V1-coded MBs;
/// **lowering** the per-MB RDO lambda there shifts more MBs to V4 and
/// recovers most of that error at modest wire-size cost. On larger
/// frames where the codebook is undersaturated, raising lambda saves
/// bytes more than it loses quality. Round 5's picker can't trade off
/// these regimes because it can only compare strip-count variants of
/// the same lambda; round 6's grid picker can.
///
/// Defaults via [`encode_rgb24_round6`] sweep `strip_candidates =
/// [1, 2, 4]` × `lambda_candidates = [Some(0.0), Some(2.5),
/// opts.rdo_lambda]` — sufficient to capture the V4-saturation win on
/// small frames and the bit-budget win on large ones at 12 trial
/// encodes total per frame.
///
/// Returns an error when either candidate list is empty.
pub fn encode_rgb24_best_rd_grid(
    rgb: &[u8],
    width: u32,
    height: u32,
    opts: EncoderOptions,
    strip_candidates: &[u16],
    lambda_candidates: &[Option<f32>],
) -> Result<Vec<u8>> {
    if strip_candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_best_rd_grid: strip_candidates must not be empty",
        ));
    }
    if lambda_candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_best_rd_grid: lambda_candidates must not be empty",
        ));
    }
    // Scoring lambda comes from `opts.rdo_lambda` (Lagrangian cost
    // ranking is independent of the per-MB RDO lambda chosen for the
    // trial encode). When `opts.rdo_lambda = None` we fall back to
    // pure-distortion ranking (lambda = 0 in cost).
    let scoring_lambda = opts.rdo_lambda.unwrap_or(0.0) as f64;
    let n_pixels = (width as usize) * (height as usize);
    let mut best: Option<(f64, Vec<u8>)> = None;
    for &strip_count in strip_candidates {
        for &per_mb_lambda in lambda_candidates {
            let trial_opts = EncoderOptions {
                strip_count,
                rdo_lambda: per_mb_lambda,
                ..opts
            };
            let bytes = encode_rgb24(rgb, width, height, trial_opts)?;
            let mut dec = crate::decoder::CinepakDecoder::new();
            let frame = dec.decode_frame(&bytes, None)?;
            let stride = frame.stride();
            let pixels = frame.pixels();
            let mut sum_sq: f64 = 0.0;
            for r in 0..height as usize {
                let off_dec = r * stride;
                let off_src = r * (width as usize) * 3;
                for c in 0..(width as usize) * 3 {
                    let d = pixels[off_dec + c] as f64 - rgb[off_src + c] as f64;
                    sum_sq += d * d;
                }
            }
            let d_per_pixel = sum_sq / (n_pixels as f64 * 3.0);
            let r_bytes = bytes.len() as f64;
            let cost = d_per_pixel + scoring_lambda * r_bytes / n_pixels as f64;
            let better = match &best {
                None => true,
                Some((best_cost, best_bytes)) => {
                    cost < *best_cost || (cost == *best_cost && r_bytes < best_bytes.len() as f64)
                }
            };
            if better {
                best = Some((cost, bytes));
            }
        }
    }
    Ok(best
        .expect("at least one (strips, lambda) trial processed")
        .1)
}

/// Round-6 convenience wrapper for [`encode_rgb24_best_rd_grid`] with
/// the default 4×3 = 12-trial grid: `strip_candidates = [1, 2, 4]` ×
/// `lambda_candidates = [Some(0.0), Some(2.5), opts.rdo_lambda]`. Picks
/// the encoding that minimises `R/N + opts.rdo_lambda · D/N` (= the
/// scoring metric of [`encode_rgb24_best_strips`]) under the round-6
/// post-classification Lloyd polish (Lever H, default-on via
/// `EncoderOptions::pcl_max_iter`).
///
/// On the round-5 64×64 gradient fixture, `encode_rgb24_round6` lifts
/// PSNR_Y from r5's 42.39 dB (at 2554 B) to ≥ 42.9 dB (at comparable
/// wire size) — meeting the round-6 ≥ 0.5 dB headline target. On the
/// 320×240 gradient it lifts from r5's 41.70 dB to ≥ 43.0 dB at
/// comparable size.
pub fn encode_rgb24_round6(
    rgb: &[u8],
    width: u32,
    height: u32,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    let strip_candidates = [1u16, 2, 4];
    let lambda_candidates: [Option<f32>; 3] = [
        Some(0.0_f32),
        Some(2.5_f32),
        Some(opts.rdo_lambda.unwrap_or(5.0_f32)),
    ];
    // Deduplicate identical lambda candidates while preserving order so
    // the trial count stays at ≤ 3 (essential when opts.rdo_lambda is
    // already 0.0 or 2.5 — duplicates would only waste compute, not
    // produce worse picks).
    let mut dedup: Vec<Option<f32>> = Vec::with_capacity(3);
    for &lam in &lambda_candidates {
        if !dedup.contains(&lam) {
            dedup.push(lam);
        }
    }
    encode_rgb24_best_rd_grid(rgb, width, height, opts, &strip_candidates, &dedup)
}

/// Round-7 (Levers J + K) three-axis RD grid picker.
///
/// Trial-encodes the input at every `(strip_count, rdo_lambda,
/// luma_weight)` combination from the
/// `strip_candidates` × `lambda_candidates` × `luma_candidates`
/// cross-product and returns the bitstream minimising the Y-channel
/// Lagrangian cost `D_Y/N + opts.rdo_lambda · R/N`, where:
///
/// - `D_Y/N` is BT.601 **Y-channel SSE per pixel** between the source
///   RGB buffer and the self-decoded RGB output
///   (`Y = 0.299 R + 0.587 G + 0.114 B`).
/// - `R/N` is the encoded wire size in bytes per pixel.
///
/// ## Lever J — `luma_weight` axis (third axis)
///
/// [`encode_rgb24_best_rd_grid`] sweeps `(strip_count, rdo_lambda)` but
/// freezes `luma_weight` at `opts.luma_weight` (default `2`). Different
/// fixtures favour different `luma_weight` values: the 64×64 gradient at
/// `q=50` likes `luma_weight = 4` (+1.14 dB over `lw=2`), the 320×240
/// gradient likes `luma_weight = 16` at the same wire footprint
/// (45.30 dB at 8889 B vs 45.25 dB at 14586 B for `lw=2`). Adding the
/// axis lets the picker pivot per-content and per-frame instead of
/// requiring the caller to guess.
///
/// ## Lever K — **Y-channel scoring distortion**
///
/// [`encode_rgb24_best_strips`] and [`encode_rgb24_best_rd_grid`] score
/// candidates by **RGB SSE** per pixel-channel — but the headline
/// quality metric for the project is **PSNR_Y** (BT.601 Y-channel mean
/// squared error). Higher `luma_weight` improves Y at the cost of
/// chroma, so RGB-SSE scoring actively penalises the `luma_weight`
/// values that boost PSNR_Y the most — defeating Lever J. Y-channel
/// scoring aligns the picker's optimisation target with the headline
/// metric. (RGB-scoring pickers stay available via
/// [`encode_rgb24_best_strips`] / [`encode_rgb24_best_rd_grid`] for
/// callers that care about chroma fidelity.)
///
/// Cost: O(|strips| × |lambdas| × |luma|) trial encodes per frame.
/// Defaults via [`encode_rgb24_round7`] sweep `[1, 2, 4]` × `[Some(0.0),
/// Some(2.5), opts.rdo_lambda]` × `[opts.luma_weight, 4, 8]` for ≤ 27
/// trial encodes (after dedup-by-luma_weight ≤ 9 trials per dedup-by-lambda
/// pair). Tests show actual measured cost at 21–27 trial encodes per
/// frame on small fixtures.
///
/// Returns an error when any candidate list is empty.
pub fn encode_rgb24_best_rd_grid_3axis(
    rgb: &[u8],
    width: u32,
    height: u32,
    opts: EncoderOptions,
    strip_candidates: &[u16],
    lambda_candidates: &[Option<f32>],
    luma_candidates: &[u8],
) -> Result<Vec<u8>> {
    if strip_candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_best_rd_grid_3axis: strip_candidates must not be empty",
        ));
    }
    if lambda_candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_best_rd_grid_3axis: lambda_candidates must not be empty",
        ));
    }
    if luma_candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_best_rd_grid_3axis: luma_candidates must not be empty",
        ));
    }
    let scoring_lambda = opts.rdo_lambda.unwrap_or(0.0) as f64;
    let n_pixels = (width as usize) * (height as usize);
    let mut best: Option<(f64, Vec<u8>)> = None;
    for &strip_count in strip_candidates {
        for &per_mb_lambda in lambda_candidates {
            for &luma_weight in luma_candidates {
                let trial_opts = EncoderOptions {
                    strip_count,
                    rdo_lambda: per_mb_lambda,
                    luma_weight,
                    ..opts
                };
                let bytes = encode_rgb24(rgb, width, height, trial_opts)?;
                let mut dec = crate::decoder::CinepakDecoder::new();
                let frame = dec.decode_frame(&bytes, None)?;
                let stride = frame.stride();
                let pixels = frame.pixels();
                // Y-channel SSE per pixel (BT.601 luma weights). See
                // Lever K rationale above.
                let mut sum_sq: f64 = 0.0;
                for r in 0..height as usize {
                    let off_dec = r * stride;
                    let off_src = r * (width as usize) * 3;
                    for c in 0..(width as usize) {
                        let r_a = rgb[off_src + c * 3] as f64;
                        let g_a = rgb[off_src + c * 3 + 1] as f64;
                        let b_a = rgb[off_src + c * 3 + 2] as f64;
                        let r_b = pixels[off_dec + c * 3] as f64;
                        let g_b = pixels[off_dec + c * 3 + 1] as f64;
                        let b_b = pixels[off_dec + c * 3 + 2] as f64;
                        let ya = 0.299 * r_a + 0.587 * g_a + 0.114 * b_a;
                        let yb = 0.299 * r_b + 0.587 * g_b + 0.114 * b_b;
                        let d = ya - yb;
                        sum_sq += d * d;
                    }
                }
                let d_per_pixel = sum_sq / n_pixels as f64;
                let r_bytes = bytes.len() as f64;
                let cost = d_per_pixel + scoring_lambda * r_bytes / n_pixels as f64;
                let better = match &best {
                    None => true,
                    Some((best_cost, best_bytes)) => {
                        cost < *best_cost
                            || (cost == *best_cost && r_bytes < best_bytes.len() as f64)
                    }
                };
                if better {
                    best = Some((cost, bytes));
                }
            }
        }
    }
    Ok(best
        .expect("at least one (strips, lambda, luma) trial processed")
        .1)
}

/// Round-7 convenience wrapper for [`encode_rgb24_best_rd_grid_3axis`]
/// with the default 3×3×3 = 27-trial grid (deduplicated):
///
/// - `strip_candidates = [1, 2, 4]`
/// - `lambda_candidates = [Some(0.0), Some(2.5), opts.rdo_lambda]` (deduped)
/// - `luma_candidates = [opts.luma_weight, 4, 8]` (deduped)
///
/// Picks the encoding that minimises **Y-channel SSE per pixel** plus
/// the Lagrangian byte cost (`opts.rdo_lambda · R/N`), so the picker's
/// optimisation target matches the project's headline PSNR_Y metric.
///
/// On the 64×64 gradient fixture at `q=50`, `encode_rgb24_round7` lifts
/// PSNR_Y from r6's 43.44 dB (at 2704 B via `encode_rgb24_round6`) to
/// **≥ 44.4 dB** (at ~2944 B) — a ≥ 0.5 dB headline gain meeting the
/// round-7 target. On the 320×240 gradient it shifts from r6's 45.25 dB
/// at 14586 B to ≥ 45.30 dB at ~8900 B (similar quality at roughly 60%
/// of the wire size) by selecting the high-`luma_weight` operating
/// point that r6's RGB-scoring picker discarded.
pub fn encode_rgb24_round7(
    rgb: &[u8],
    width: u32,
    height: u32,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    let strip_candidates = [1u16, 2, 4];
    let lambda_seed: [Option<f32>; 3] = [
        Some(0.0_f32),
        Some(2.5_f32),
        Some(opts.rdo_lambda.unwrap_or(5.0_f32)),
    ];
    let mut lambdas: Vec<Option<f32>> = Vec::with_capacity(3);
    for &lam in &lambda_seed {
        if !lambdas.contains(&lam) {
            lambdas.push(lam);
        }
    }
    let luma_seed: [u8; 3] = [opts.luma_weight.max(1), 4, 8];
    let mut lumas: Vec<u8> = Vec::with_capacity(3);
    for &lw in &luma_seed {
        if !lumas.contains(&lw) {
            lumas.push(lw);
        }
    }
    encode_rgb24_best_rd_grid_3axis(
        rgb,
        width,
        height,
        opts,
        &strip_candidates,
        &lambdas,
        &lumas,
    )
}

/// Round-8 (Lever L) **per-strip independent (lambda, luma_weight) picker.**
///
/// The round-7 picker
/// ([`encode_rgb24_best_rd_grid_3axis`] / [`encode_rgb24_round7`])
/// trial-encodes the **entire frame** with a single `(strip_count,
/// rdo_lambda, luma_weight)` per trial, then picks the lowest-cost
/// trial. The chosen `(rdo_lambda, luma_weight)` then applies to
/// **every** strip of the frame.
///
/// That's wasteful on **split-content** frames where different strips
/// have qualitatively different pixel statistics — e.g. a top half of
/// smooth-gradient sky (best PSNR_Y at high `luma_weight = 8`) and a
/// bottom half of high-chroma diverse texture (best PSNR_Y at low
/// `luma_weight = 1` so the chroma codebook stays informative). The
/// round-7 picker must choose **one** `luma_weight` for the whole
/// frame, compromising on whichever strip is the "loser".
///
/// Cinepak's bitstream lets each strip carry its own pair of codebooks
/// trained independently (per spec §3.4 of `02-codebooks.md`), so
/// per-strip `luma_weight` and `rdo_lambda` are first-class: the
/// decoder doesn't care which lever values the encoder picked per
/// strip, only that each strip's codebook is consistent with the
/// strip's vector chunk indices.
///
/// **What this picker does**: for each `strip_count` candidate, plan
/// the strips, then for each strip independently sweep every
/// `(lambda, luma_weight)` combination and pick the one minimising
/// per-strip Y-channel SSE (BT.601 luma weights) plus the Lagrangian
/// byte cost. Assemble the winning per-strip bitstreams. Across
/// `strip_count` candidates, return the assembled frame with the
/// lowest total cost.
///
/// **Composition with prior levers**: each per-strip trial encode
/// inherits the full opts (PCL polish, LBG passes, Lloyd refinement)
/// — Lever L is strictly a picker-layer addition on top of the
/// existing codebook-training stack. Cross-frame persistence is
/// disabled in per-strip trials (the trial encoder is constructed
/// per-strip with no prior codebook state, by design — Lever L is
/// intra-only).
///
/// **Cost**: O(|strips| × |lambdas| × |lumas|) trial encodes per
/// `strip_count` candidate, plus one final per-strip encode. For the
/// default 3-strip-counts × 3-lambdas × 3-lumas grid via
/// [`encode_rgb24_round8`] on a 4-strip frame this is up to
/// `3 + (2 + 4) × 9 + (3 + 5) = 65` trial encodes per frame
/// (`strip_count=1` is the round-7-Y picker; per-strip kicks in for
/// `strip_count ≥ 2`).
///
/// Returns an error when any candidate list is empty.
pub fn encode_rgb24_per_strip_rd(
    rgb: &[u8],
    width: u32,
    height: u32,
    opts: EncoderOptions,
    strip_candidates: &[u16],
    lambda_candidates: &[Option<f32>],
    luma_candidates: &[u8],
) -> Result<Vec<u8>> {
    if strip_candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_per_strip_rd: strip_candidates must not be empty",
        ));
    }
    if lambda_candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_per_strip_rd: lambda_candidates must not be empty",
        ));
    }
    if luma_candidates.is_empty() {
        return Err(CinepakError::other(
            "encode_rgb24_per_strip_rd: luma_candidates must not be empty",
        ));
    }
    validate_dims(width, height)?;
    validate_input_size(rgb, width, height, PixelMode::Yuv12)?;

    let scoring_lambda = opts.rdo_lambda.unwrap_or(0.0) as f64;
    let n_pixels_frame = (width as usize) * (height as usize);

    // For each strip_count candidate, pick the best per-strip config
    // independently, then assemble. Track the overall lowest-cost
    // assembled bytestream.
    let mb_rows = (height / 4) as usize;
    let mut best_frame: Option<(f64, Vec<u8>)> = None;
    for &strip_count in strip_candidates {
        let strips = plan_strips(mb_rows, strip_count as usize);
        // For each strip independently, sweep (lambda, luma) and pick
        // the per-strip Y-SSE + λ·R minimiser. Encode each candidate
        // standalone as a single-strip frame of size (width, strip_h),
        // decode it, measure Y-SSE against the strip's input rows.
        let mut chosen_strip_bytes: Vec<Vec<u8>> = Vec::with_capacity(strips.len());
        let mut frame_y_sse: f64 = 0.0;
        let mut frame_bytes_for_cost: f64 = 0.0;
        let mut frame_ok = true;
        for s in &strips {
            let strip_h = s.y_bottom - s.y_top;
            // Extract this strip's pixels.
            let strip_pixels = extract_strip_rgb(rgb, width, s.y_top, strip_h);
            // Per-strip trial loop.
            let mut best_strip: Option<(f64, Vec<u8>, f64, f64)> = None;
            for &per_mb_lambda in lambda_candidates {
                for &luma_weight in luma_candidates {
                    let trial_opts = EncoderOptions {
                        strip_count: 1,
                        rdo_lambda: per_mb_lambda,
                        luma_weight,
                        ..opts
                    };
                    // Encode standalone (y_top = 0, single strip).
                    let trial_bytes = match encode_rgb24(&strip_pixels, width, strip_h, trial_opts)
                    {
                        Ok(b) => b,
                        // Skip invalid combinations silently (e.g.
                        // strip_h too small for some opt) but never
                        // bubble up — we want a best-effort pick.
                        Err(_) => continue,
                    };
                    let mut dec = crate::decoder::CinepakDecoder::new();
                    let frame = match dec.decode_frame(&trial_bytes, None) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    let stride = frame.stride();
                    let pixels = frame.pixels();
                    // Y-channel SSE between source strip and decoded
                    // strip (both are (width, strip_h) RGB24).
                    let mut sum_sq: f64 = 0.0;
                    for r in 0..strip_h as usize {
                        let off_dec = r * stride;
                        let off_src = r * (width as usize) * 3;
                        for c in 0..(width as usize) {
                            let r_a = strip_pixels[off_src + c * 3] as f64;
                            let g_a = strip_pixels[off_src + c * 3 + 1] as f64;
                            let b_a = strip_pixels[off_src + c * 3 + 2] as f64;
                            let r_b = pixels[off_dec + c * 3] as f64;
                            let g_b = pixels[off_dec + c * 3 + 1] as f64;
                            let b_b = pixels[off_dec + c * 3 + 2] as f64;
                            let ya = 0.299 * r_a + 0.587 * g_a + 0.114 * b_a;
                            let yb = 0.299 * r_b + 0.587 * g_b + 0.114 * b_b;
                            let d = ya - yb;
                            sum_sq += d * d;
                        }
                    }
                    let n_strip_pixels = (width as usize) * (strip_h as usize);
                    let d_per_pixel = sum_sq / n_strip_pixels as f64;
                    let r_bytes = trial_bytes.len() as f64;
                    let cost = d_per_pixel + scoring_lambda * r_bytes / n_strip_pixels as f64;
                    let better = match &best_strip {
                        None => true,
                        Some((best_cost, _, _, _)) => cost < *best_cost,
                    };
                    if better {
                        best_strip = Some((cost, trial_bytes, sum_sq, r_bytes));
                    }
                }
            }
            let Some((_, _trial_bytes, strip_sse, strip_r_bytes)) = best_strip else {
                // No candidate succeeded for this strip — fall back to
                // a baseline encode of just this strip (sweep with the
                // caller's default opts). If even that fails, abandon
                // this strip_count candidate.
                let fallback = encode_rgb24(
                    &strip_pixels,
                    width,
                    strip_h,
                    EncoderOptions {
                        strip_count: 1,
                        ..opts
                    },
                );
                match fallback {
                    Ok(_) => {
                        // Re-encode with the real strip plan and continue;
                        // strip_sse / r_bytes set to dummy that won't win.
                        let mut roll = RollingCodebooks::default();
                        let real = encode_intra_strip(
                            rgb,
                            width,
                            PixelMode::Yuv12,
                            &EncoderOptions {
                                strip_count: 1,
                                ..opts
                            },
                            s,
                            &mut roll,
                        )?;
                        chosen_strip_bytes.push(real);
                        frame_ok = true;
                    }
                    Err(_) => {
                        frame_ok = false;
                        break;
                    }
                }
                continue;
            };
            // Re-encode the chosen (lambda, luma_weight) for THIS strip
            // with the real strip plan (y_top set correctly). The
            // standalone trial bytes have y_top = 0 in the strip header
            // and can't be concatenated into a multi-strip frame
            // directly. Re-encoding costs one extra encode per strip,
            // but the codebook training is deterministic — the final
            // strip is bit-identical content to the trial up to the
            // strip header's y_top / y_bottom.
            //
            // Determine the winning trial's (lambda, lw) by searching
            // again at the same options: the trial loop above didn't
            // record them in the best_strip tuple to keep the per-trial
            // overhead minimal. Instead, replay the search using the
            // recorded cost to identify the winning (lambda, lw).
            let (winning_lambda, winning_lw) = find_winning_opts(
                &strip_pixels,
                width,
                strip_h,
                opts,
                lambda_candidates,
                luma_candidates,
                scoring_lambda,
            )?;
            let final_opts = EncoderOptions {
                strip_count: 1,
                rdo_lambda: winning_lambda,
                luma_weight: winning_lw,
                ..opts
            };
            let mut roll = RollingCodebooks::default();
            let real_strip =
                encode_intra_strip(rgb, width, PixelMode::Yuv12, &final_opts, s, &mut roll)?;
            frame_y_sse += strip_sse;
            frame_bytes_for_cost += strip_r_bytes;
            chosen_strip_bytes.push(real_strip);
        }
        if !frame_ok {
            continue;
        }
        // Assemble the multi-strip frame.
        let assembled = assemble_frame(width, height, &chosen_strip_bytes)?;
        // Score the assembled frame on a frame-level Y-SSE / R basis
        // (note: the per-strip cost above is in per-strip units; we
        // need a frame-level comparable cost across strip_count
        // candidates).
        let d_per_pixel = frame_y_sse / n_pixels_frame as f64;
        let r_bytes = assembled.len() as f64;
        let frame_cost = d_per_pixel + scoring_lambda * r_bytes / n_pixels_frame as f64;
        // tiebreak toward smaller wire size.
        let better = match &best_frame {
            None => true,
            Some((best_cost, best_bytes)) => {
                frame_cost < *best_cost
                    || (frame_cost == *best_cost && r_bytes < best_bytes.len() as f64)
            }
        };
        let _ = frame_bytes_for_cost;
        if better {
            best_frame = Some((frame_cost, assembled));
        }
    }
    Ok(best_frame
        .ok_or_else(|| {
            CinepakError::other("encode_rgb24_per_strip_rd: no strip_count candidate succeeded")
        })?
        .1)
}

/// Round-8 convenience wrapper for [`encode_rgb24_per_strip_rd`] with
/// the same default 3×3×3 sweep as [`encode_rgb24_round7`]: per-strip
/// `(lambda, luma_weight)` selection across all strips, then assemble.
///
/// - `strip_candidates = [1, 2, 4]`
/// - `lambda_candidates = [Some(0.0), Some(2.5), opts.rdo_lambda]` (deduped)
/// - `luma_candidates = [opts.luma_weight, 4, 8]` (deduped)
///
/// On split-content fixtures where different strips favour different
/// `(lambda, luma_weight)` regimes, the per-strip picker lifts
/// PSNR_Y over the round-7 frame-uniform picker without growing wire
/// size (each strip's per-MB classifier saturates at the per-strip
/// optimum rather than at the frame-uniform compromise). On
/// homogeneous fixtures (e.g. the 64×64 single-gradient) it matches
/// round-7 within noise — every strip prefers the same regime so the
/// per-strip pick degenerates to the frame-uniform pick.
pub fn encode_rgb24_round8(
    rgb: &[u8],
    width: u32,
    height: u32,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    let strip_candidates = [1u16, 2, 4];
    let lambda_seed: [Option<f32>; 3] = [
        Some(0.0_f32),
        Some(2.5_f32),
        Some(opts.rdo_lambda.unwrap_or(5.0_f32)),
    ];
    let mut lambdas: Vec<Option<f32>> = Vec::with_capacity(3);
    for &lam in &lambda_seed {
        if !lambdas.contains(&lam) {
            lambdas.push(lam);
        }
    }
    let luma_seed: [u8; 3] = [opts.luma_weight.max(1), 4, 8];
    let mut lumas: Vec<u8> = Vec::with_capacity(3);
    for &lw in &luma_seed {
        if !lumas.contains(&lw) {
            lumas.push(lw);
        }
    }
    // Round-8 guarantees ≥ round-7 by trying BOTH pickers and keeping
    // the lower-cost result. The per-strip greedy can occasionally lose
    // to the frame-uniform pick when strip-local optima don't compose to
    // a frame-level optimum (the per-strip scoring metric is per-strip
    // Y-SSE per pixel + λ·R/N_strip, but the cross-candidate scoring is
    // frame-level D/N_frame + λ·R/N_frame — the per-strip greedy isn't
    // guaranteed to minimise the frame-level cost).
    let per_strip = encode_rgb24_per_strip_rd(
        rgb,
        width,
        height,
        opts,
        &strip_candidates,
        &lambdas,
        &lumas,
    )?;
    let frame_uniform = encode_rgb24_round7(rgb, width, height, opts)?;
    let scoring_lambda = opts.rdo_lambda.unwrap_or(0.0) as f64;
    let n_pixels = (width as usize) * (height as usize);
    let cost_of = |bytes: &[u8]| -> Result<f64> {
        let mut dec = crate::decoder::CinepakDecoder::new();
        let frame = dec.decode_frame(bytes, None)?;
        let stride = frame.stride();
        let pixels = frame.pixels();
        let mut sum_sq: f64 = 0.0;
        for r in 0..height as usize {
            let off_dec = r * stride;
            let off_src = r * (width as usize) * 3;
            for c in 0..(width as usize) {
                let r_a = rgb[off_src + c * 3] as f64;
                let g_a = rgb[off_src + c * 3 + 1] as f64;
                let b_a = rgb[off_src + c * 3 + 2] as f64;
                let r_b = pixels[off_dec + c * 3] as f64;
                let g_b = pixels[off_dec + c * 3 + 1] as f64;
                let b_b = pixels[off_dec + c * 3 + 2] as f64;
                let ya = 0.299 * r_a + 0.587 * g_a + 0.114 * b_a;
                let yb = 0.299 * r_b + 0.587 * g_b + 0.114 * b_b;
                let d = ya - yb;
                sum_sq += d * d;
            }
        }
        let d_per_pixel = sum_sq / n_pixels as f64;
        Ok(d_per_pixel + scoring_lambda * bytes.len() as f64 / n_pixels as f64)
    };
    let c_ps = cost_of(&per_strip)?;
    let c_fu = cost_of(&frame_uniform)?;
    // Tiebreak: smaller wire wins. Otherwise prefer per_strip (richer
    // search; tied costs typically mean the per-strip greedy converged
    // to the frame-uniform pick).
    if c_ps < c_fu || (c_ps == c_fu && per_strip.len() <= frame_uniform.len()) {
        Ok(per_strip)
    } else {
        Ok(frame_uniform)
    }
}

/// Extract a horizontal strip of pixels from a packed `Rgb24` buffer
/// into a fresh `(width × strip_h × 3)` byte vector. Used by the
/// round-8 per-strip picker to feed each strip into a standalone
/// `encode_rgb24` trial.
fn extract_strip_rgb(rgb: &[u8], width: u32, y_top: u32, strip_h: u32) -> Vec<u8> {
    let stride = (width as usize) * 3;
    let start = (y_top as usize) * stride;
    let end = ((y_top + strip_h) as usize) * stride;
    rgb[start..end].to_vec()
}

/// Replay the per-strip RD sweep to identify the winning `(lambda,
/// luma_weight)` pair for one strip. Used by
/// [`encode_rgb24_per_strip_rd`] after a winning per-strip trial has
/// been identified by cost: we need the actual `(lambda, lw)` values to
/// re-encode the strip with the real strip plan (y_top correct).
///
/// Separated out for clarity of [`encode_rgb24_per_strip_rd`]'s main
/// loop; identical scoring metric (Y-SSE per pixel + λ·R/N) so the
/// winning pair always matches the inner loop's pick.
fn find_winning_opts(
    strip_pixels: &[u8],
    width: u32,
    strip_h: u32,
    opts: EncoderOptions,
    lambda_candidates: &[Option<f32>],
    luma_candidates: &[u8],
    scoring_lambda: f64,
) -> Result<(Option<f32>, u8)> {
    let mut best: Option<(f64, Option<f32>, u8)> = None;
    let n_strip_pixels = (width as usize) * (strip_h as usize);
    for &per_mb_lambda in lambda_candidates {
        for &luma_weight in luma_candidates {
            let trial_opts = EncoderOptions {
                strip_count: 1,
                rdo_lambda: per_mb_lambda,
                luma_weight,
                ..opts
            };
            let trial_bytes = match encode_rgb24(strip_pixels, width, strip_h, trial_opts) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let mut dec = crate::decoder::CinepakDecoder::new();
            let frame = match dec.decode_frame(&trial_bytes, None) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let stride = frame.stride();
            let pixels = frame.pixels();
            let mut sum_sq: f64 = 0.0;
            for r in 0..strip_h as usize {
                let off_dec = r * stride;
                let off_src = r * (width as usize) * 3;
                for c in 0..(width as usize) {
                    let r_a = strip_pixels[off_src + c * 3] as f64;
                    let g_a = strip_pixels[off_src + c * 3 + 1] as f64;
                    let b_a = strip_pixels[off_src + c * 3 + 2] as f64;
                    let r_b = pixels[off_dec + c * 3] as f64;
                    let g_b = pixels[off_dec + c * 3 + 1] as f64;
                    let b_b = pixels[off_dec + c * 3 + 2] as f64;
                    let ya = 0.299 * r_a + 0.587 * g_a + 0.114 * b_a;
                    let yb = 0.299 * r_b + 0.587 * g_b + 0.114 * b_b;
                    let d = ya - yb;
                    sum_sq += d * d;
                }
            }
            let d_per_pixel = sum_sq / n_strip_pixels as f64;
            let r_bytes = trial_bytes.len() as f64;
            let cost = d_per_pixel + scoring_lambda * r_bytes / n_strip_pixels as f64;
            let better = match &best {
                None => true,
                Some((best_cost, _, _)) => cost < *best_cost,
            };
            if better {
                best = Some((cost, per_mb_lambda, luma_weight));
            }
        }
    }
    let (_, lam, lw) = best.ok_or_else(|| {
        CinepakError::other("find_winning_opts: no candidate succeeded for strip")
    })?;
    Ok((lam, lw))
}

/// Stateful Cinepak encoder. Tracks the rolling V4/V1 codebook the
/// decoder will hold across strips and frames so that **inter** frames
/// can emit selective-update codebook chunks (`0x2100` / `0x2300` /
/// `0x2500` / `0x2700`) — and omit codebook chunks entirely when the
/// previous codebook is already correct for the strip's referenced
/// slots — instead of always emitting full-replace.
///
/// Use [`CinepakEncoder::encode_intra`] to start a sequence (or to
/// inject a keyframe) and [`CinepakEncoder::encode_inter`] for
/// subsequent frames. The encoder also tracks the previous
/// reconstructed frame internally for SKIP-MB selection, so callers
/// don't need to thread it through.
///
/// On a perfectly-static fixture (input pixels unchanged across
/// frames), `encode_inter` after the second frame typically emits
/// **no** codebook chunks at all — only an empty `0x3100` vector
/// chunk full of SKIP codes. This is the headline wire-size win
/// motivating the round-4 design.
///
/// ## Cross-frame codebook persistence (round 5)
///
/// On `encode_inter`, the median-cut quantiser is warm-started with
/// the previous frame's codebook centroids (one Lloyd refinement
/// pass): each new vector is assigned to the slot of its nearest
/// prior centroid, and that slot's new centroid is the average of the
/// vectors that landed there. Slots with no incoming vectors retain
/// the prior centroid byte-identical, which lets the chunk-omission
/// path keep firing on slow-pan content where most macroblocks shift
/// but the codebook population is roughly stable. Disable via
/// [`Self::set_cross_frame_codebook_persistence`] for A/B
/// measurements.
pub struct CinepakEncoder {
    rolling: RollingCodebooks,
    /// Previous reconstructed frame, decoded back from our last
    /// emitted bitstream. Required for `encode_inter`'s SKIP-MB
    /// detection.
    prev_frame: Option<CinepakFrame>,
    /// Internal decoder used to reconstruct the previous frame from
    /// our own emitted bytes. Held so its prev-frame state is in sync
    /// with what an external receiver-side decoder will see.
    decoder: crate::decoder::CinepakDecoder,
    /// Round-5 cross-frame codebook persistence toggle. Default `true`.
    cross_frame_persistence: bool,
    /// Round-7 telemetry: counters from the most recent
    /// `encode_intra` / `encode_inter` call. Reset at the start of
    /// each frame.
    last_stats: FrameStats,
    /// Round-96 bitrate-target rate control: per-frame byte budget. When
    /// `Some(n)`, [`Self::encode_intra`] / [`Self::encode_inter`] drive
    /// the three-axis `(strip_count, rdo_lambda, luma_weight)` RD grid
    /// toward `≤ n` bytes per frame instead of using the fixed `opts`
    /// the caller passes. `None` reproduces the legacy quality-controlled
    /// behaviour (the caller's `opts` are used verbatim, single trial).
    target_frame_bytes: Option<usize>,
    /// Round-96 telemetry: outcome of the most recent budget-driven
    /// frame. `None` when the encoder is not in target-bitrate mode.
    last_rate_stats: Option<RateStats>,
}

/// Round-7 per-frame telemetry, queried via
/// [`CinepakEncoder::last_frame_stats`] after each
/// [`CinepakEncoder::encode_intra`] / [`CinepakEncoder::encode_inter`]
/// call. Counters are reset at the start of each frame and accumulate
/// across all of the frame's strips (so a 4-strip frame can produce up
/// to 4 reclamations per codebook flavour).
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    /// Number of V4 codebook slots reclaimed via round-7 empty-cluster
    /// recovery during the most recent frame. Zero on intra frames or
    /// when reclamation is disabled (`stale_slot_threshold = None`).
    pub reclaimed_v4_slots: usize,
    /// Number of V1 codebook slots reclaimed via round-7 empty-cluster
    /// recovery during the most recent frame.
    pub reclaimed_v1_slots: usize,
    /// Number of full-replace codebook chunks the encoder was forced
    /// to emit due to slot reclamation in the most recent frame.
    /// Maxes at `2 × strip_count` (one per V4/V1 codebook per strip).
    pub forced_full_chunks: usize,
}

/// Round-96 per-frame rate-control telemetry, queried via
/// [`CinepakEncoder::last_rate_stats`] after a budget-driven
/// [`CinepakEncoder::encode_intra`] / [`CinepakEncoder::encode_inter`]
/// call. `None` (via the accessor) when the encoder is not in
/// target-bitrate mode.
#[derive(Clone, Copy, Debug)]
pub struct RateStats {
    /// The per-frame byte budget in effect for the most recent frame
    /// (`bits_per_second / 8 / fps`, or the directly-set byte budget).
    pub target_bytes: usize,
    /// The encoded size of the frame the picker committed to.
    pub actual_bytes: usize,
    /// `actual_bytes as i64 - target_bytes as i64`. Negative ⇒ under
    /// budget (the common case — the picker takes the highest-quality
    /// grid point that still fits). Positive ⇒ overshoot: even the
    /// smallest grid candidate exceeded the budget, so the smallest was
    /// emitted and the budget could not be honoured.
    pub byte_delta: i64,
    /// `true` when the committed frame fits the budget
    /// (`actual_bytes ≤ target_bytes`).
    pub within_budget: bool,
    /// Number of grid candidates trial-encoded for this frame.
    pub trials: usize,
}

impl Default for CinepakEncoder {
    fn default() -> Self {
        Self {
            rolling: RollingCodebooks::default(),
            prev_frame: None,
            decoder: crate::decoder::CinepakDecoder::new(),
            cross_frame_persistence: true,
            last_stats: FrameStats::default(),
            target_frame_bytes: None,
            last_rate_stats: None,
        }
    }
}

impl CinepakEncoder {
    /// Construct a fresh encoder. The first call must be
    /// [`Self::encode_intra`] (no prev frame for SKIP comparison).
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all per-frame carry-over state (rolling codebooks, prev
    /// frame, telemetry). The encoder behaves as if it had never seen a
    /// frame — the next call must be [`Self::encode_intra`]. Preserves
    /// the cross-frame persistence flag **and** the target-bitrate /
    /// per-frame byte budget configuration, so a configured rate-control
    /// encoder can be re-used across independent sequences without
    /// re-applying [`Self::with_target_bitrate`].
    pub fn reset(&mut self) {
        let cf = self.cross_frame_persistence;
        let tgt = self.target_frame_bytes;
        *self = Self::default();
        self.cross_frame_persistence = cf;
        self.target_frame_bytes = tgt;
    }

    /// Toggle round-5 cross-frame codebook persistence (warm-starting
    /// the median-cut quantiser with the previous frame's centroids).
    /// Default `true`.
    ///
    /// Useful for A/B wire-size measurements; for production encoding
    /// the default is the recommended setting.
    pub fn set_cross_frame_codebook_persistence(&mut self, on: bool) {
        self.cross_frame_persistence = on;
    }

    /// Whether cross-frame codebook persistence is currently enabled.
    pub fn cross_frame_codebook_persistence(&self) -> bool {
        self.cross_frame_persistence
    }

    /// Round-96 bitrate-target rate control (builder form).
    ///
    /// Switches the encoder into **target-bitrate mode**: each subsequent
    /// [`Self::encode_intra`] / [`Self::encode_inter`] call drives the
    /// three-axis `(strip_count, rdo_lambda, luma_weight)` RD grid picker
    /// toward a **per-frame byte budget** of
    /// `bits_per_second / 8 / fps` bytes, instead of using the caller's
    /// `EncoderOptions` verbatim.
    ///
    /// The per-frame budget is the constant-bitrate slice of one frame's
    /// worth of the target stream rate. For a 256 kbit/s stream at 15
    /// fps the budget is `256_000 / 8 / 15 ≈ 2133` bytes per frame.
    ///
    /// ## How the budget steers the grid
    ///
    /// For each frame the encoder sweeps the same 3×3×3 (deduplicated)
    /// grid as [`encode_rgb24_round7`] — `strip_count ∈ {1, 2, 4}`,
    /// `rdo_lambda ∈ {0.0, 2.5, opts.rdo_lambda}`, `luma_weight ∈
    /// {opts.luma_weight, 4, 8}` — but the **selection rule changes**:
    /// rather than minimising the Lagrangian `D + λ·R`, the picker keeps
    /// the candidate with the **lowest Y-channel SSE whose encoded size
    /// is `≤ budget`** (best quality that fits). When *no* candidate
    /// fits (even the smallest grid point overshoots) it commits the
    /// **smallest** candidate and reports a positive
    /// [`RateStats::byte_delta`] — the API never errors on overshoot,
    /// mirroring the ICER `with_byte_budget` contract.
    ///
    /// Because the grid only spans a finite quality range, very small
    /// budgets on large frames can be unsatisfiable; the smallest grid
    /// point (highest `rdo_lambda`, most V1 macroblocks) is the floor.
    /// Inspect [`Self::last_rate_stats`] after each frame to confirm
    /// adherence.
    ///
    /// The grid candidates inherit the rest of the caller's `opts`
    /// (LBG passes, Lloyd refinement, PCL polish, k-means++ seeding,
    /// skip threshold) and the encoder's cross-frame persistence + prev
    /// frame state, so inter frames still skip and selectively-update
    /// against the committed previous frame.
    ///
    /// `fps` is clamped to `≥ 1.0`; `bits_per_second` of `0` yields a
    /// budget of `0` (every frame overshoots, smallest grid point
    /// always chosen). Pass `f64` fps to support fractional rates like
    /// 23.976.
    ///
    /// Call with the resulting budget recoverable via
    /// [`Self::target_frame_bytes`]. Use [`Self::clear_target_bitrate`]
    /// (or construct a fresh encoder) to return to quality-controlled
    /// mode.
    #[must_use]
    pub fn with_target_bitrate(mut self, bits_per_second: u64, fps: f64) -> Self {
        self.set_target_bitrate(bits_per_second, fps);
        self
    }

    /// In-place form of [`Self::with_target_bitrate`].
    pub fn set_target_bitrate(&mut self, bits_per_second: u64, fps: f64) {
        let fps = if fps.is_finite() { fps.max(1.0) } else { 1.0 };
        // bytes/frame = (bits/s) / 8 / (frames/s). Round to nearest.
        let bytes = ((bits_per_second as f64) / 8.0 / fps).round();
        let bytes = if bytes.is_finite() && bytes >= 0.0 {
            bytes as usize
        } else {
            0
        };
        self.target_frame_bytes = Some(bytes);
    }

    /// Round-96 builder: set the per-frame byte budget directly (instead
    /// of deriving it from a bitrate + fps pair). Equivalent to
    /// [`Self::with_target_bitrate`] but the caller supplies the
    /// bytes-per-frame figure. Useful when the budget comes from a
    /// container's per-frame allowance rather than a constant bitrate.
    #[must_use]
    pub fn with_target_frame_bytes(mut self, bytes_per_frame: usize) -> Self {
        self.target_frame_bytes = Some(bytes_per_frame);
        self
    }

    /// In-place form of [`Self::with_target_frame_bytes`].
    pub fn set_target_frame_bytes(&mut self, bytes_per_frame: usize) {
        self.target_frame_bytes = Some(bytes_per_frame);
    }

    /// The per-frame byte budget currently in effect, or `None` when the
    /// encoder is in quality-controlled mode.
    pub fn target_frame_bytes(&self) -> Option<usize> {
        self.target_frame_bytes
    }

    /// Disable target-bitrate mode; subsequent frames use the caller's
    /// `EncoderOptions` verbatim (legacy quality-controlled behaviour).
    pub fn clear_target_bitrate(&mut self) {
        self.target_frame_bytes = None;
        self.last_rate_stats = None;
    }

    /// Round-96 telemetry: outcome of the most recent budget-driven
    /// frame, or `None` when the encoder is not in target-bitrate mode
    /// (or no frame has been encoded yet in that mode). See
    /// [`RateStats`] for the per-field semantics.
    pub fn last_rate_stats(&self) -> Option<RateStats> {
        self.last_rate_stats
    }

    /// Round-7 telemetry: per-frame counters accumulated during the
    /// most recent [`Self::encode_intra`] / [`Self::encode_inter`]
    /// call. See [`FrameStats`] for the per-counter semantics.
    ///
    /// Reset at the start of each frame, so calling this between
    /// `encode_inter` invocations always returns the previous frame's
    /// values; calling it before the first frame returns
    /// [`FrameStats::default`] (all zeros).
    pub fn last_frame_stats(&self) -> FrameStats {
        self.last_stats
    }

    /// Encode an intra frame from packed `Rgb24` input. Resets the
    /// rolling codebook state (intra strips always emit full-replace
    /// codebook chunks anyway, but this also clears any stale
    /// selective-update reference).
    ///
    /// When the encoder is in **target-bitrate mode** (configured via
    /// [`Self::with_target_bitrate`] / [`Self::with_target_frame_bytes`])
    /// the `opts` argument is treated as a *base*: the three-axis RD grid
    /// is swept around it and the highest-quality candidate that fits the
    /// per-frame byte budget is committed. See
    /// [`Self::with_target_bitrate`] for the selection rule, and
    /// [`Self::last_rate_stats`] for the per-frame adherence telemetry.
    pub fn encode_intra(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        opts: EncoderOptions,
    ) -> Result<Vec<u8>> {
        validate_dims(width, height)?;
        validate_opts(&opts)?;
        validate_input_size(rgb, width, height, PixelMode::Yuv12)?;
        if self.target_frame_bytes.is_some() {
            return self.encode_budget_frame(rgb, width, height, opts, true);
        }
        self.encode_intra_one(rgb, width, height, opts)
    }

    /// Quality-controlled intra encode worker: commits the frame coded
    /// with exactly `opts` to `self`'s rolling/prev state and returns the
    /// bytes. This is the legacy [`Self::encode_intra`] body; the
    /// budget-driven path calls it once per grid trial.
    fn encode_intra_one(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        opts: EncoderOptions,
    ) -> Result<Vec<u8>> {
        let mb_rows = (height / 4) as usize;
        let strips = plan_strips(mb_rows, opts.strip_count as usize);
        let mut frame_strips: Vec<Vec<u8>> = Vec::with_capacity(strips.len());
        // Intra resets rolling: intra strips always emit full-replace.
        // Round-7: this also zeros all per-slot staleness counters.
        self.rolling = RollingCodebooks::default();
        // Reset round-7 telemetry; intra never reclaims (no rolling
        // staleness state to consult anyway), so the counters stay zero
        // unless something below changes (it doesn't).
        self.last_stats = FrameStats::default();
        for s in &strips {
            let bytes =
                encode_intra_strip(rgb, width, PixelMode::Yuv12, &opts, s, &mut self.rolling)?;
            frame_strips.push(bytes);
        }
        let bytes = assemble_frame(width, height, &frame_strips)?;
        // Reconstruct frame for next-call SKIP-MB selection.
        self.decoder.reset();
        let f = self.decoder.decode_frame(&bytes, None)?;
        self.prev_frame = Some(f);
        Ok(bytes)
    }

    /// Encode an inter frame from packed `Rgb24` input, referencing
    /// the previously-emitted frame for SKIP-MB selection and using
    /// the rolling codebook state for selective-update emission.
    ///
    /// Returns an error if no prior frame has been emitted (the first
    /// call must be [`Self::encode_intra`]).
    ///
    /// When the encoder is in **target-bitrate mode** the three-axis RD
    /// grid is swept around `opts` per frame and the highest-quality
    /// candidate that fits the per-frame byte budget is committed; see
    /// [`Self::with_target_bitrate`].
    pub fn encode_inter(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        opts: EncoderOptions,
    ) -> Result<Vec<u8>> {
        validate_dims(width, height)?;
        validate_opts(&opts)?;
        validate_input_size(rgb, width, height, PixelMode::Yuv12)?;

        let prev = self.prev_frame.as_ref().ok_or_else(|| {
            CinepakError::other("encoder: encode_inter called before encode_intra; no prev frame")
        })?;
        if prev.width != width || prev.height != height {
            return Err(CinepakError::other(format!(
                "encoder: prev frame dims {}x{} != current {width}x{height}",
                prev.width, prev.height
            )));
        }
        if self.target_frame_bytes.is_some() {
            return self.encode_budget_frame(rgb, width, height, opts, false);
        }
        self.encode_inter_one(rgb, width, height, opts)
    }

    /// Quality-controlled inter encode worker: commits the frame coded
    /// with exactly `opts` to `self`'s rolling/prev state and returns the
    /// bytes. This is the legacy [`Self::encode_inter`] body; the
    /// budget-driven path calls it once per grid trial. The caller has
    /// already validated dims/opts and confirmed a prev frame exists.
    fn encode_inter_one(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        opts: EncoderOptions,
    ) -> Result<Vec<u8>> {
        let mb_rows = (height / 4) as usize;
        let strips = plan_strips(mb_rows, opts.strip_count as usize);
        let mut frame_strips: Vec<Vec<u8>> = Vec::with_capacity(strips.len());
        let prev = self.prev_frame.as_ref().unwrap().clone();
        // Round-7 telemetry: reset before this frame; encode_inter_strip_with_stats
        // will accumulate into `acc`.
        let mut acc = FrameStatsAccum::default();
        for s in &strips {
            let bytes = encode_inter_strip_with_stats(
                rgb,
                &prev,
                width,
                PixelMode::Yuv12,
                &opts,
                s,
                &mut self.rolling,
                true,
                self.cross_frame_persistence,
                Some(&mut acc),
            )?;
            frame_strips.push(bytes);
        }
        // Round-7: now that all strips have run, update per-slot
        // staleness using the per-frame OR of all strips' `used`
        // masks. This is the per-frame increment semantic — multi-strip
        // frames don't bump staleness once per strip, only once per
        // frame.
        self.rolling.update_staleness(
            WhichCodebook::V4,
            &acc.frame_v4_used,
            opts.v4_entries as usize,
        );
        self.rolling.update_staleness(
            WhichCodebook::V1,
            &acc.frame_v1_used,
            opts.v1_entries as usize,
        );
        self.last_stats = FrameStats {
            reclaimed_v4_slots: acc.reclaimed_v4_slots,
            reclaimed_v1_slots: acc.reclaimed_v1_slots,
            forced_full_chunks: acc.forced_full_chunks,
        };
        let bytes = assemble_frame(width, height, &frame_strips)?;
        // Reconstruct frame for next-call SKIP-MB selection. The
        // internal decoder picks up from where it left off, so its
        // codebook + prev-frame state stays in sync with an external
        // receiver-side decoder.
        let f = self.decoder.decode_frame(&bytes, None)?;
        self.prev_frame = Some(f);
        Ok(bytes)
    }

    /// Snapshot the per-frame carry-over state (rolling codebooks, prev
    /// frame, internal decoder, telemetry) so a budget grid sweep can
    /// trial-encode each candidate from the *same* starting point and
    /// restore between trials. Excludes the rate-control configuration
    /// (budget / persistence flag), which the sweep never mutates.
    fn snapshot_state(&self) -> EncoderSnapshot {
        EncoderSnapshot {
            rolling: self.rolling.clone(),
            prev_frame: self.prev_frame.clone(),
            decoder: self.decoder.clone(),
            last_stats: self.last_stats,
        }
    }

    /// Restore a [`Self::snapshot_state`] result.
    fn restore_state(&mut self, snap: EncoderSnapshot) {
        self.rolling = snap.rolling;
        self.prev_frame = snap.prev_frame;
        self.decoder = snap.decoder;
        self.last_stats = snap.last_stats;
    }

    /// Round-96 budget-driven frame encode. Sweeps the three-axis
    /// `(strip_count, rdo_lambda, luma_weight)` RD grid around `base`,
    /// trial-encodes each candidate against a snapshot of the encoder's
    /// current carry-over state, and commits the highest-quality
    /// candidate (lowest Y-channel SSE) whose encoded size fits the
    /// per-frame byte budget. When no candidate fits, commits the
    /// smallest candidate and reports the overshoot via
    /// [`RateStats::byte_delta`] — never errors on overshoot.
    ///
    /// `intra` selects the intra (`true`) or inter (`false`) worker for
    /// each trial. Both validations have already run in the public entry.
    fn encode_budget_frame(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        base: EncoderOptions,
        intra: bool,
    ) -> Result<Vec<u8>> {
        let budget = self.target_frame_bytes.unwrap_or(usize::MAX);
        let candidates = budget_grid_candidates(base);
        // Snapshot once; every trial restores back to this point so the
        // grid candidates are scored from identical state.
        let snap = self.snapshot_state();

        // Track, in a single pass:
        //   - best_fit: lowest Y-SSE candidate whose bytes ≤ budget.
        //   - smallest: smallest-byte candidate overall (overshoot floor).
        let mut best_fit: Option<(f64, usize, EncoderOptions)> = None;
        let mut smallest: Option<(usize, EncoderOptions)> = None;
        let mut trials = 0usize;

        for opts in &candidates {
            self.restore_state(snap.clone());
            let bytes = if intra {
                self.encode_intra_one(rgb, width, height, *opts)
            } else {
                self.encode_inter_one(rgb, width, height, *opts)
            };
            let bytes = match bytes {
                Ok(b) => b,
                // A candidate combination may be invalid for this frame
                // (e.g. strip_count clamps the same as another); skip it
                // rather than abort the whole frame.
                Err(_) => continue,
            };
            trials += 1;
            let len = bytes.len();
            // Smallest tracker (used only when nothing fits).
            let take_small = match &smallest {
                None => true,
                Some((s_len, _)) => len < *s_len,
            };
            if take_small {
                smallest = Some((len, *opts));
            }
            // Best-fit tracker: only candidates within budget compete on
            // quality (Y-SSE). Tiebreak toward smaller wire size.
            if len <= budget {
                // Decode the candidate against a clone of the
                // pre-frame decoder state (held in the snapshot) so an
                // inter frame's skip / selective-update MBs resolve
                // against the correct previous reconstruction — a fresh
                // decoder would reject `0x3100` skips with no prev frame.
                let mut trial_dec = snap.decoder.clone();
                let y_sse = decode_y_sse(&mut trial_dec, &bytes, rgb, width, height)?;
                let take = match &best_fit {
                    None => true,
                    Some((b_sse, b_len, _)) => y_sse < *b_sse || (y_sse == *b_sse && len < *b_len),
                };
                if take {
                    best_fit = Some((y_sse, len, *opts));
                }
            }
        }

        // Pick the winner: best fit if any candidate fit, else smallest.
        let (winning_opts, within_budget) = match (&best_fit, &smallest) {
            (Some((_, _, o)), _) => (*o, true),
            (None, Some((_, o))) => (*o, false),
            (None, None) => {
                // Defensive: no candidate succeeded at all. Restore and
                // fall back to the base opts via the plain worker so the
                // caller still gets a frame (or a real error from it).
                self.restore_state(snap);
                return if intra {
                    self.encode_intra_one(rgb, width, height, base)
                } else {
                    self.encode_inter_one(rgb, width, height, base)
                };
            }
        };

        // Commit: replay the winning candidate from the snapshot so
        // self's carry-over state matches the emitted bytes exactly.
        self.restore_state(snap);
        let bytes = if intra {
            self.encode_intra_one(rgb, width, height, winning_opts)?
        } else {
            self.encode_inter_one(rgb, width, height, winning_opts)?
        };
        let actual = bytes.len();
        self.last_rate_stats = Some(RateStats {
            target_bytes: budget,
            actual_bytes: actual,
            byte_delta: actual as i64 - budget as i64,
            within_budget,
            trials,
        });
        Ok(bytes)
    }
}

/// Decode `bytes` through `dec` (which must hold the correct pre-frame
/// carry-over state for an inter frame) and return the BT.601 Y-channel
/// SSE between the decoded frame and the source `rgb`. Used by the
/// round-96 budget grid sweep to rank candidates that fit the byte
/// budget by quality.
fn decode_y_sse(
    dec: &mut crate::decoder::CinepakDecoder,
    bytes: &[u8],
    rgb: &[u8],
    width: u32,
    height: u32,
) -> Result<f64> {
    let frame = dec.decode_frame(bytes, None)?;
    let stride = frame.stride();
    let pixels = frame.pixels();
    let mut sum_sq: f64 = 0.0;
    for r in 0..height as usize {
        let off_dec = r * stride;
        let off_src = r * (width as usize) * 3;
        for c in 0..(width as usize) {
            let r_a = rgb[off_src + c * 3] as f64;
            let g_a = rgb[off_src + c * 3 + 1] as f64;
            let b_a = rgb[off_src + c * 3 + 2] as f64;
            let r_b = pixels[off_dec + c * 3] as f64;
            let g_b = pixels[off_dec + c * 3 + 1] as f64;
            let b_b = pixels[off_dec + c * 3 + 2] as f64;
            let ya = 0.299 * r_a + 0.587 * g_a + 0.114 * b_a;
            let yb = 0.299 * r_b + 0.587 * g_b + 0.114 * b_b;
            let d = ya - yb;
            sum_sq += d * d;
        }
    }
    Ok(sum_sq)
}

/// Snapshot of a [`CinepakEncoder`]'s per-frame carry-over state for the
/// round-96 budget grid sweep (trial-and-restore). Does not capture the
/// rate-control configuration (budget / persistence) — only the state a
/// trial encode mutates.
#[derive(Clone)]
struct EncoderSnapshot {
    rolling: RollingCodebooks,
    prev_frame: Option<CinepakFrame>,
    decoder: crate::decoder::CinepakDecoder,
    last_stats: FrameStats,
}

/// Build the round-96 budget grid candidate list around `base`: the
/// same deduplicated 3×3×3 `(strip_count, rdo_lambda, luma_weight)`
/// cross-product the round-7 picker sweeps, expressed as concrete
/// [`EncoderOptions`] (the rest of the fields inherited from `base`).
///
/// Candidates are ordered cheapest-first within each strip count so the
/// smallest-byte floor (the overshoot fallback) tends to appear early —
/// high `rdo_lambda` (more V1 macroblocks) and low strip count produce
/// the smallest frames.
fn budget_grid_candidates(base: EncoderOptions) -> Vec<EncoderOptions> {
    let strip_candidates = [1u16, 2, 4];
    let lambda_seed: [Option<f32>; 3] = [
        Some(base.rdo_lambda.unwrap_or(5.0_f32)),
        Some(2.5_f32),
        Some(0.0_f32),
    ];
    let mut lambdas: Vec<Option<f32>> = Vec::with_capacity(3);
    for &lam in &lambda_seed {
        if !lambdas.contains(&lam) {
            lambdas.push(lam);
        }
    }
    let luma_seed: [u8; 3] = [base.luma_weight.max(1), 4, 8];
    let mut lumas: Vec<u8> = Vec::with_capacity(3);
    for &lw in &luma_seed {
        if !lumas.contains(&lw) {
            lumas.push(lw);
        }
    }
    let mut out: Vec<EncoderOptions> =
        Vec::with_capacity(strip_candidates.len() * lambdas.len() * lumas.len());
    for &strip_count in &strip_candidates {
        for &rdo_lambda in &lambdas {
            for &luma_weight in &lumas {
                out.push(EncoderOptions {
                    strip_count,
                    rdo_lambda,
                    luma_weight,
                    ..base
                });
            }
        }
    }
    out
}

/// Encode an 8-bit grayscale intra frame from packed `Gray8` input
/// (`width × height` bytes).
pub fn encode_gray8(gray: &[u8], width: u32, height: u32, opts: EncoderOptions) -> Result<Vec<u8>> {
    encode_intra_frame(gray, width, height, PixelMode::Gray8, opts)
}

/// Two-pass rate control wrapping a [`CinepakEncoder`] (round 5).
///
/// Pass 1 ([`Self::stats_pass`]): drives the encoder at a fixed
/// reference quality across the full input sequence and records
/// per-frame byte counts. The aggregate gives the average frame size
/// at that quality — used as a starting point for pass-2 budgeting.
///
/// Pass 2 ([`Self::encode_at_target_bytes`]): for each frame, picks
/// the **largest** `q ∈ 0..=100` from a sparse grid whose encoded
/// byte count is `≤ target_bytes`. If no quality satisfies the
/// target (even q=0 overflows), the q=0 result is returned with a
/// positive [`RateControlledFrame::byte_delta`] — the API never
/// errors on rate overshoot.
///
/// Each per-frame quality search re-encodes the entire prior
/// sub-sequence through a throwaway encoder, so total cost is
/// O(N² × G) where N = frame count and G = grid size (11). This is
/// acceptable for offline two-pass rate control which isn't on the
/// real-time hot path; the structure is the same as ffmpeg's
/// `--passlogfile` two-pass mode but with a coarser grid.
///
/// Limitations:
///
/// - Per-frame budget — no rate smoothing across frames. The caller
///   averages `target_bytes` across a window themselves.
/// - Quality knob is coarse (codebook sizes log-spaced 8..=256), so
///   achievable byte-size resolution is also coarse.
/// - The chosen q is the largest from `[0, 10, 20, …, 100]` whose
///   bytes ≤ target; sub-quality interpolation is not attempted.
pub struct TwoPassRateControl {
    /// First-pass reference quality (0..=100). Default `50`.
    pub reference_quality: u8,
    per_frame_bytes: Vec<usize>,
}

/// Per-frame outcome from [`TwoPassRateControl::encode_at_target_bytes`].
#[derive(Clone, Debug)]
pub struct RateControlledFrame {
    /// The encoded bitstream.
    pub bytes: Vec<u8>,
    /// The quality knob the search settled on (0..=100).
    pub quality: u8,
    /// `bytes.len() - target_bytes` as `i64`. Negative ⇒ under budget,
    /// positive ⇒ over (q=0 still couldn't fit).
    pub byte_delta: i64,
}

impl Default for TwoPassRateControl {
    fn default() -> Self {
        Self {
            reference_quality: 50,
            per_frame_bytes: Vec::new(),
        }
    }
}

impl TwoPassRateControl {
    /// Construct a fresh two-pass controller.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stats-collection pass: encode the full sequence at the
    /// reference quality and record per-frame byte counts. Returns
    /// the total wire size at that quality.
    ///
    /// The first frame in `frames` is encoded as intra; subsequent
    /// frames are encoded as inter against the encoder's rolling
    /// state. Each `frame` is `(width × height × 3)` bytes of packed
    /// `Rgb24`.
    pub fn stats_pass(&mut self, frames: &[Vec<u8>], width: u32, height: u32) -> Result<usize> {
        self.per_frame_bytes.clear();
        let opts = EncoderOptions::from_quality(self.reference_quality);
        let mut enc = CinepakEncoder::new();
        let mut total = 0usize;
        for (i, f) in frames.iter().enumerate() {
            let bytes = if i == 0 {
                enc.encode_intra(f, width, height, opts)?
            } else {
                enc.encode_inter(f, width, height, opts)?
            };
            total += bytes.len();
            self.per_frame_bytes.push(bytes.len());
        }
        Ok(total)
    }

    /// Per-frame byte counts from the most recent
    /// [`Self::stats_pass`]. Empty until a stats pass runs.
    pub fn per_frame_bytes(&self) -> &[usize] {
        &self.per_frame_bytes
    }

    /// Average per-frame bytes across the most recent stats pass.
    /// Returns `None` if `stats_pass` has not been called or yielded
    /// no frames.
    pub fn average_frame_bytes(&self) -> Option<f64> {
        if self.per_frame_bytes.is_empty() {
            None
        } else {
            let total: usize = self.per_frame_bytes.iter().sum();
            Some(total as f64 / self.per_frame_bytes.len() as f64)
        }
    }

    /// Round-6 windowed bisection rate control.
    ///
    /// Encodes `frames` while targeting a byte budget over a rolling
    /// window of `window_size` frames (e.g. `target_window_bytes =
    /// bitrate_bps × window_size / 8 / fps_per_window`). The encoder
    /// holds quality at the previous frame's chosen `q` while the
    /// rolling sum of the last `window_size` frame sizes stays within
    /// ±`tolerance_pct` of `target_window_bytes`; when it drifts
    /// outside, the encoder runs a binary search over `q ∈ [0, 100]`
    /// for the next frame to pull the window back toward target.
    ///
    /// Compared with [`Self::encode_at_target_bytes`] (per-frame grid
    /// search), the windowed path:
    ///
    /// - Targets a window-budget rather than per-frame budget — real
    ///   bitrate-throttled workloads only care about the rolling
    ///   average, not the per-frame size.
    /// - Holds quality steady when in-band — fewer per-frame quality
    ///   transitions in the output stream.
    /// - Uses bisection (≤ 7 trials per re-evaluation) instead of an
    ///   11-point grid → tighter quality resolution on the same
    ///   per-frame replay budget.
    ///
    /// The first frame always re-evaluates quality (no history). The
    /// returned [`RateControlledFrame::byte_delta`] is computed against
    /// `target_window_bytes / window_size` — the per-frame slice of
    /// the window budget — for compatibility with the per-frame API.
    ///
    /// Panics in debug builds if `window_size == 0` or `tolerance_pct
    /// < 0.0`.
    pub fn encode_at_target_window_bytes(
        &self,
        frames: &[Vec<u8>],
        width: u32,
        height: u32,
        target_window_bytes: usize,
        window_size: usize,
        tolerance_pct: f32,
    ) -> Result<Vec<RateControlledFrame>> {
        debug_assert!(window_size > 0, "window_size must be ≥ 1");
        debug_assert!(tolerance_pct >= 0.0, "tolerance_pct must be ≥ 0");
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let window = window_size.max(1);
        let tol = tolerance_pct.max(0.0);
        let per_frame_target = target_window_bytes / window;

        let mut chosen_qs: Vec<u8> = Vec::with_capacity(frames.len());
        let mut frame_sizes: Vec<usize> = Vec::with_capacity(frames.len());
        let mut out: Vec<RateControlledFrame> = Vec::with_capacity(frames.len());
        let mut current_q: u8 = 50; // Mid-quality starting point.

        for frame_idx in 0..frames.len() {
            // Recent-window byte sum (excluding the current frame).
            let lo = frame_idx.saturating_sub(window);
            let win_sum: usize = frame_sizes[lo..frame_idx].iter().sum();

            // Decide whether to bisect or hold.
            // - First frame: always bisect (no prior chosen_q).
            // - Otherwise: bisect only when projected window sum
            //   deviates from target by more than tol%.
            let must_bisect = if frame_idx == 0 {
                true
            } else {
                // Project the window sum if we re-used current_q: use
                // the avg of past in-window frame sizes as the
                // estimate (cheap; bisection itself does the precise
                // job when triggered).
                let in_win = frame_idx - lo;
                let est_next = win_sum.checked_div(in_win).unwrap_or(per_frame_target);
                let projected = win_sum + est_next;
                let projected_target = target_window_bytes
                    .saturating_sub(window.saturating_sub(in_win + 1) * per_frame_target);
                let drift_pct = if projected_target > 0 {
                    let d = projected as i64 - projected_target as i64;
                    d.unsigned_abs() as f32 * 100.0 / projected_target as f32
                } else {
                    f32::INFINITY
                };
                drift_pct > tol
            };

            let (chosen_q, chosen_bytes, chosen_len) = if must_bisect {
                bisect_q_for_window(
                    frames,
                    width,
                    height,
                    &chosen_qs,
                    frame_idx,
                    win_sum,
                    target_window_bytes,
                    window,
                )?
            } else {
                let bytes =
                    replay_to_frame_at_q(frames, width, height, &chosen_qs, frame_idx, current_q)?;
                let len = bytes.len();
                (current_q, bytes, len)
            };

            chosen_qs.push(chosen_q);
            frame_sizes.push(chosen_len);
            current_q = chosen_q;
            out.push(RateControlledFrame {
                bytes: chosen_bytes,
                quality: chosen_q,
                byte_delta: chosen_len as i64 - per_frame_target as i64,
            });
        }
        Ok(out)
    }

    /// Round-7 adaptive-tolerance windowed bisection.
    ///
    /// Identical to [`Self::encode_at_target_window_bytes`] but couples
    /// the per-frame "are we drifting?" tolerance to the **running
    /// stdev** of the prior `window` frames' actual byte counts.
    /// Tighter rate control during stable scenes (low byte-size
    /// variance ⇒ tolerance shrinks toward `tolerance_pct_min`) and
    /// looser control during scene changes / wipes (high variance ⇒
    /// tolerance grows toward `tolerance_pct_max`). Without this,
    /// round-6's fixed tolerance is either too tight on volatile
    /// content (bisects every frame, wasting trials) or too loose on
    /// stable content (lets drift accumulate).
    ///
    /// The effective tolerance is computed per-frame as:
    ///
    /// ```text
    /// tol = tolerance_pct_min
    ///     + (tolerance_pct_max - tolerance_pct_min)
    ///     × clamp(stdev_pct / variance_scale_pct, 0, 1)
    /// ```
    ///
    /// where `stdev_pct = stdev(window_sizes) / mean(window_sizes) ×
    /// 100`. `variance_scale_pct` is the stdev-pct above which
    /// tolerance saturates at the upper bound (default `25.0` — a
    /// common scene-change variance).
    ///
    /// Until the encoder has produced `≥ 2` prior frames there's no
    /// meaningful stdev; the first two frames use the upper bound
    /// (`tolerance_pct_max`) so the controller bisects opportunistically
    /// during start-up.
    ///
    /// Compared with the fixed-tolerance variant on a 6-frame
    /// slow-pan-then-cut fixture: tolerance settles to ~ min during
    /// the slow pan (controller holds quality steady at 1 bisection
    /// per ~5 frames), then jumps to max at the cut (controller
    /// re-bisects on the cut frame and the one after to converge to
    /// the new content's byte profile).
    #[allow(clippy::too_many_arguments)]
    pub fn encode_at_target_window_bytes_adaptive(
        &self,
        frames: &[Vec<u8>],
        width: u32,
        height: u32,
        target_window_bytes: usize,
        window_size: usize,
        tolerance_pct_min: f32,
        tolerance_pct_max: f32,
        variance_scale_pct: f32,
    ) -> Result<Vec<RateControlledFrame>> {
        debug_assert!(window_size > 0, "window_size must be ≥ 1");
        debug_assert!(tolerance_pct_min >= 0.0, "tolerance_pct_min must be ≥ 0");
        debug_assert!(
            tolerance_pct_max >= tolerance_pct_min,
            "tolerance_pct_max must be ≥ tolerance_pct_min"
        );
        debug_assert!(variance_scale_pct > 0.0, "variance_scale_pct must be > 0");
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let window = window_size.max(1);
        let per_frame_target = target_window_bytes / window;

        let mut chosen_qs: Vec<u8> = Vec::with_capacity(frames.len());
        let mut frame_sizes: Vec<usize> = Vec::with_capacity(frames.len());
        let mut out: Vec<RateControlledFrame> = Vec::with_capacity(frames.len());
        let mut current_q: u8 = 50;

        for frame_idx in 0..frames.len() {
            let lo = frame_idx.saturating_sub(window);
            let win_sum: usize = frame_sizes[lo..frame_idx].iter().sum();
            let in_win = frame_idx - lo;

            // Compute the effective tolerance for this frame from the
            // running stdev of the prior window's byte sizes. Use the
            // upper bound when we don't have enough history yet.
            let tol = if in_win >= 2 {
                let mean = win_sum as f64 / in_win as f64;
                let var: f64 = frame_sizes[lo..frame_idx]
                    .iter()
                    .map(|&s| {
                        let d = s as f64 - mean;
                        d * d
                    })
                    .sum::<f64>()
                    / in_win as f64;
                let stdev = var.sqrt();
                let stdev_pct = if mean > 0.0 {
                    (stdev / mean) * 100.0
                } else {
                    0.0
                };
                let scale = (stdev_pct as f32 / variance_scale_pct).clamp(0.0, 1.0);
                tolerance_pct_min + (tolerance_pct_max - tolerance_pct_min) * scale
            } else {
                tolerance_pct_max
            };

            let must_bisect = if frame_idx == 0 {
                true
            } else {
                let est_next = win_sum.checked_div(in_win).unwrap_or(per_frame_target);
                let projected = win_sum + est_next;
                let projected_target = target_window_bytes
                    .saturating_sub(window.saturating_sub(in_win + 1) * per_frame_target);
                let drift_pct = if projected_target > 0 {
                    let d = projected as i64 - projected_target as i64;
                    d.unsigned_abs() as f32 * 100.0 / projected_target as f32
                } else {
                    f32::INFINITY
                };
                drift_pct > tol
            };

            let (chosen_q, chosen_bytes, chosen_len) = if must_bisect {
                bisect_q_for_window(
                    frames,
                    width,
                    height,
                    &chosen_qs,
                    frame_idx,
                    win_sum,
                    target_window_bytes,
                    window,
                )?
            } else {
                let bytes =
                    replay_to_frame_at_q(frames, width, height, &chosen_qs, frame_idx, current_q)?;
                let len = bytes.len();
                (current_q, bytes, len)
            };

            chosen_qs.push(chosen_q);
            frame_sizes.push(chosen_len);
            current_q = chosen_q;
            out.push(RateControlledFrame {
                bytes: chosen_bytes,
                quality: chosen_q,
                byte_delta: chosen_len as i64 - per_frame_target as i64,
            });
        }
        Ok(out)
    }

    /// Pass-2: encode `frames` with each frame's quality chosen by a
    /// grid search over `EncoderOptions::from_quality(q)` for
    /// `q ∈ {0, 10, 20, …, 100}`, picking the **largest** `q` whose
    /// encoded byte count is `≤ target_bytes`.
    ///
    /// Returns one [`RateControlledFrame`] per input frame. The
    /// `bytes` field is the encoded stream at the chosen quality;
    /// concatenating them in order produces a decodable Cinepak
    /// sequence whose decoder state matches the encoder trajectory at
    /// the chosen per-frame qualities.
    pub fn encode_at_target_bytes(
        &self,
        frames: &[Vec<u8>],
        width: u32,
        height: u32,
        target_bytes: usize,
    ) -> Result<Vec<RateControlledFrame>> {
        let trial_qs: [u8; 11] = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        let mut chosen_qs: Vec<u8> = Vec::with_capacity(frames.len());
        let mut out: Vec<RateControlledFrame> = Vec::with_capacity(frames.len());
        for frame_idx in 0..frames.len() {
            // Find the largest q whose encode of frame `frame_idx`
            // (with all prior frames at their already-chosen q) is
            // ≤ target_bytes.
            let mut best: Option<(u8, Vec<u8>, usize)> = None;
            for &q in &trial_qs {
                let bytes = replay_to_frame_at_q(frames, width, height, &chosen_qs, frame_idx, q)?;
                let len = bytes.len();
                if len <= target_bytes {
                    // Largest-q-under-budget wins — overwrite when q
                    // is larger.
                    let take = best
                        .as_ref()
                        .map(|(prev_q, _, _)| q > *prev_q)
                        .unwrap_or(true);
                    if take {
                        best = Some((q, bytes, len));
                    }
                }
            }
            let (chosen_q, chosen_bytes, chosen_len) = match best {
                Some(b) => b,
                None => {
                    // Nothing fits — encode at q=0 and report
                    // overshoot.
                    let bytes =
                        replay_to_frame_at_q(frames, width, height, &chosen_qs, frame_idx, 0)?;
                    let len = bytes.len();
                    (0u8, bytes, len)
                }
            };
            chosen_qs.push(chosen_q);
            out.push(RateControlledFrame {
                bytes: chosen_bytes,
                quality: chosen_q,
                byte_delta: chosen_len as i64 - target_bytes as i64,
            });
        }
        Ok(out)
    }
}

/// Round-6 bisection: find the largest `q ∈ [0, 100]` whose encoded
/// frame size, when added to `win_sum_before`, keeps the rolling
/// `window`-frame sum at or below `target_window_bytes`.
///
/// Falls back to `q=0` (smallest possible frame) when even the lowest
/// quality overflows the projected window — caller treats the
/// overshoot as positive `byte_delta`.
///
/// Cost: ≤ 8 replay-encodes per call (binary search over 0..=100 with
/// step granularity 1 bottoms out in 7 trials; we cap at 8 for
/// safety). Each replay is O(frame_idx) full-encodes through prior
/// frames.
#[allow(clippy::too_many_arguments)]
fn bisect_q_for_window(
    frames: &[Vec<u8>],
    width: u32,
    height: u32,
    chosen_qs: &[u8],
    frame_idx: usize,
    win_sum_before: usize,
    target_window_bytes: usize,
    window: usize,
) -> Result<(u8, Vec<u8>, usize)> {
    // Per-frame slice of remaining window budget. The first
    // `window-1` frames in a window have 1 frame's allowance each;
    // once at steady state, each step rolls one frame off and admits
    // a new one with 1 frame's allowance ≈ target_window_bytes /
    // window.
    let per_frame_target = target_window_bytes / window.max(1);
    // Projected post-frame window sum if we add a frame of size `b`:
    // win_sum_before + b, evicting nothing (since we're still inside
    // the window). For a strict window we'd evict the oldest member
    // of `frame_sizes` once the window is full, but the caller
    // already passes `win_sum` over `lo..frame_idx` (length ≤
    // `window`), so adding `b` here is the correct projected sum.
    let cap = target_window_bytes.saturating_sub(win_sum_before);

    let mut lo: u8 = 0;
    let mut hi: u8 = 100;
    let mut best: Option<(u8, Vec<u8>, usize)> = None;
    // Up to 8 bisection trials: ceil(log2(101)) = 7.
    for _ in 0..8 {
        if lo > hi {
            break;
        }
        let mid = lo + (hi - lo) / 2;
        let bytes = replay_to_frame_at_q(frames, width, height, chosen_qs, frame_idx, mid)?;
        let len = bytes.len();
        if len <= cap {
            // Fits — try higher.
            best = Some((mid, bytes, len));
            if mid == 100 {
                break;
            }
            lo = mid + 1;
        } else if mid == 0 {
            // Even q=0 overflows; record it as the best-effort
            // smallest-frame option.
            if best.is_none() {
                best = Some((mid, bytes, len));
            }
            break;
        } else {
            hi = mid - 1;
        }
    }
    if let Some(b) = best {
        return Ok(b);
    }
    // Defensive: bisection never landed on a tested q. Run q=0 as a
    // last resort so caller has a frame.
    let bytes = replay_to_frame_at_q(frames, width, height, chosen_qs, frame_idx, 0)?;
    let len = bytes.len();
    let _ = per_frame_target; // suppress unused warning (used by docs)
    Ok((0u8, bytes, len))
}

/// Replay frames `0..frame_idx` at their already-chosen qualities,
/// then encode frame `frame_idx` at quality `q`. Returns the encoded
/// bytes for frame `frame_idx`.
///
/// Cost: O(frame_idx) encodes per call. Used by
/// [`TwoPassRateControl::encode_at_target_bytes`] to do per-frame
/// quality grid search without exposing internal encoder state.
fn replay_to_frame_at_q(
    frames: &[Vec<u8>],
    width: u32,
    height: u32,
    chosen_qs: &[u8],
    frame_idx: usize,
    q: u8,
) -> Result<Vec<u8>> {
    let mut enc = CinepakEncoder::new();
    // Replay 0..frame_idx at their chosen qualities.
    for i in 0..frame_idx {
        let opts = EncoderOptions::from_quality(chosen_qs[i]);
        if i == 0 {
            enc.encode_intra(&frames[i], width, height, opts)?;
        } else {
            enc.encode_inter(&frames[i], width, height, opts)?;
        }
    }
    // Encode frame_idx at q.
    let opts = EncoderOptions::from_quality(q);
    let bytes = if frame_idx == 0 {
        enc.encode_intra(&frames[frame_idx], width, height, opts)?
    } else {
        enc.encode_inter(&frames[frame_idx], width, height, opts)?
    };
    Ok(bytes)
}

/// Encode a 12-bit YUV inter frame from packed `Rgb24` input,
/// referencing `prev` for SKIP-MB selection.
///
/// `prev` must be an `Rgb24` reconstructed frame of the same dimensions
/// as `(width, height)` — typically the previous frame this encoder
/// emitted, decoded back through [`crate::CinepakDecoder`].
///
/// The encoder emits a single `0x1100` strip (or several, per
/// `opts.strip_count`) carrying full-replace codebook chunks and a
/// `0x3100` mixed-inter vector chunk. Macroblocks whose per-pixel MSE
/// against the same-position `prev` block is below
/// `opts.skip_threshold` are coded as SKIP.
pub fn encode_rgb24_inter(
    rgb: &[u8],
    prev: &CinepakFrame,
    width: u32,
    height: u32,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    if prev.width != width || prev.height != height {
        return Err(CinepakError::other(format!(
            "encoder: prev frame dims {}x{} != current {width}x{height}",
            prev.width, prev.height
        )));
    }
    if prev.pixel_format != CinepakPixelFormat::Rgb24 {
        return Err(CinepakError::other(
            "encoder: encode_rgb24_inter requires Rgb24 prev frame",
        ));
    }
    encode_inter_frame(rgb, prev, width, height, PixelMode::Yuv12, opts)
}

// ---------------------------------------------------------------------------
// Intra encode
// ---------------------------------------------------------------------------

fn encode_intra_frame(
    pixels: &[u8],
    width: u32,
    height: u32,
    mode: PixelMode,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    validate_dims(width, height)?;
    validate_opts(&opts)?;
    validate_input_size(pixels, width, height, mode)?;

    let mb_rows = (height / 4) as usize;
    let strips = plan_strips(mb_rows, opts.strip_count as usize);

    let mut frame_strips: Vec<Vec<u8>> = Vec::with_capacity(strips.len());
    let mut roll = RollingCodebooks::default();
    for s in &strips {
        let bytes = encode_intra_strip(pixels, width, mode, &opts, s, &mut roll)?;
        frame_strips.push(bytes);
    }
    assemble_frame(width, height, &frame_strips)
}

// ---------------------------------------------------------------------------
// Inter encode
// ---------------------------------------------------------------------------

fn encode_inter_frame(
    pixels: &[u8],
    prev: &CinepakFrame,
    width: u32,
    height: u32,
    mode: PixelMode,
    opts: EncoderOptions,
) -> Result<Vec<u8>> {
    validate_dims(width, height)?;
    validate_opts(&opts)?;
    validate_input_size(pixels, width, height, mode)?;

    let mb_rows = (height / 4) as usize;
    let strips = plan_strips(mb_rows, opts.strip_count as usize);

    // Stateless free-function path: no rolling codebook context, so
    // every strip emits full-replace codebook chunks. Cross-frame
    // seeding is also disabled here — the free-function path has no
    // notion of "previous frame's codebook", only "previous frame's
    // pixels".
    let mut frame_strips: Vec<Vec<u8>> = Vec::with_capacity(strips.len());
    let mut roll = RollingCodebooks::default();
    for s in &strips {
        let bytes =
            encode_inter_strip(pixels, prev, width, mode, &opts, s, &mut roll, false, false)?;
        frame_strips.push(bytes);
    }
    assemble_frame(width, height, &frame_strips)
}

// ---------------------------------------------------------------------------
// Per-strip encoders
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct StripPlan {
    /// Pixel-coordinate top of the strip (inclusive).
    y_top: u32,
    /// Pixel-coordinate bottom of the strip (exclusive).
    y_bottom: u32,
}

fn plan_strips(mb_rows: usize, requested: usize) -> Vec<StripPlan> {
    let n = requested.clamp(1, mb_rows.max(1));
    // Distribute MB rows as evenly as possible; remainder goes to
    // the first `mb_rows % n` strips.
    let base = mb_rows / n;
    let rem = mb_rows % n;
    let mut out = Vec::with_capacity(n);
    let mut y_mb = 0usize;
    for i in 0..n {
        let h_mb = base + if i < rem { 1 } else { 0 };
        let y_top = (y_mb * 4) as u32;
        let y_bot = ((y_mb + h_mb) * 4) as u32;
        out.push(StripPlan {
            y_top,
            y_bottom: y_bot,
        });
        y_mb += h_mb;
    }
    out
}

fn encode_intra_strip(
    pixels: &[u8],
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
    roll: &mut RollingCodebooks,
) -> Result<Vec<u8>> {
    // Intra strips emit a fresh codebook with no cross-frame seeding —
    // the decoder drops its codebook state on intra anyway, so seeding
    // would only spend training cycles biasing toward stale centroids
    // that the decoder won't have. (Intra-within-frame across strips
    // also doesn't seed: each strip carries its own full-replace chunk
    // and the decoder treats each strip's codebook as authoritative.)
    let plan = build_codebooks_and_decisions(pixels, width, mode, opts, s, None, false, None)?;
    let StripPlanResult { v4_cb, v1_cb, mbs } = plan;

    let mut chunks = Vec::new();
    emit_codebook_chunks_full(mode, &v4_cb, &v1_cb, opts, &mut chunks);

    // Vector chunk 0x3000 (mixed intra).
    let mut vec_payload = Vec::new();
    encode_mixed_intra_payload(&mbs, &mut vec_payload)?;
    let vec_chunk_size = (CHUNK_HEADER_SIZE + vec_payload.len()) as u16;
    chunks.extend_from_slice(&VECTOR_CHUNK_INTRA.to_be_bytes());
    chunks.extend_from_slice(&vec_chunk_size.to_be_bytes());
    chunks.extend_from_slice(&vec_payload);

    // Update rolling state — intra always replaces both codebooks
    // wholesale at sizes `opts.v?_entries`.
    roll.set(WhichCodebook::V4, v4_cb, opts.v4_entries as usize);
    roll.set(WhichCodebook::V1, v1_cb, opts.v1_entries as usize);

    finalise_strip(STRIP_ID_INTRA, s, width, &chunks)
}

#[allow(clippy::too_many_arguments)]
fn encode_inter_strip(
    pixels: &[u8],
    prev: &CinepakFrame,
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
    roll: &mut RollingCodebooks,
    selective_enabled: bool,
    cross_frame_seeding: bool,
) -> Result<Vec<u8>> {
    encode_inter_strip_with_stats(
        pixels,
        prev,
        width,
        mode,
        opts,
        s,
        roll,
        selective_enabled,
        cross_frame_seeding,
        None,
    )
}

/// Internal sibling of [`encode_inter_strip`] that also accumulates
/// per-strip telemetry into `stats`, when `Some`. The free-function /
/// stateless paths pass `None`; the stateful [`CinepakEncoder`] passes
/// `Some(&mut stats)` so it can expose round-7 reclamation counts /
/// forced-full counts to callers.
#[allow(clippy::too_many_arguments)]
fn encode_inter_strip_with_stats(
    pixels: &[u8],
    prev: &CinepakFrame,
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
    roll: &mut RollingCodebooks,
    selective_enabled: bool,
    cross_frame_seeding: bool,
    stats: Option<&mut FrameStatsAccum>,
) -> Result<Vec<u8>> {
    // Cross-frame codebook persistence (round 5): seed the median-cut
    // quantiser with the rolling codebook the decoder currently holds.
    // When the previous codebook entry count matches `opts.v?_entries`
    // we feed its centroids into median_cut so newly-trained slots track
    // the slot indices the decoder already references — slots that
    // remain stable across frames stay byte-identical, which lets
    // `emit_codebook_chunk_one` either emit no chunk (chunk-omission)
    // or shrink the selective-update slot list. On slow-pan content
    // this dramatically shortens the inter-strip wire size.
    //
    // The seed is also valid *across strips* of the same frame: by the
    // time strip 2 runs, `roll` carries strip 1's emitted codebook, so
    // strip 2's median-cut anchors to strip 1's slot identities. This
    // is what makes selective-update wire-cheaper than full-replace
    // within a single multi-strip inter frame.
    let mut seed_opt = if cross_frame_seeding {
        let v4_seed = roll
            .get(WhichCodebook::V4)
            .filter(|(_, n)| *n == opts.v4_entries as usize)
            .map(|(cb, _)| cb.clone());
        let v1_seed = roll
            .get(WhichCodebook::V1)
            .filter(|(_, n)| *n == opts.v1_entries as usize)
            .map(|(cb, _)| cb.clone());
        Some(SeedCodebooks {
            v4: v4_seed,
            v1: v1_seed,
        })
    } else {
        None
    };

    // Round-7 empty-cluster slot reclamation (PRIMARY round-7 feature).
    //
    // When a slot has been unreferenced for `≥ stale_slot_threshold`
    // consecutive inter frames, its persistent (warm-started)
    // centroid has gone stale — it's surviving cross-frame persistence
    // but no longer represents any current content. We "reclaim" the
    // slot by overwriting its seed centroid with a high-residual
    // sample MB from this strip *before* Lloyd refinement kicks in;
    // Lloyd's nearest-seed assignment then naturally pulls a small
    // outlier cluster onto the reclaimed slot, retraining it for
    // current content.
    //
    // Reclamation only applies when:
    // - `opts.stale_slot_threshold` is `Some(n)`.
    // - `cross_frame_seeding` is true (otherwise there's no seed to
    //   patch and the cold-start median-cut already retrains everything).
    // - The seed for the codebook flavour matches the requested entry
    //   count (`v?_seed.is_some()`).
    //
    // After reclamation, `force_full_*` is set so the codebook chunk
    // emits full-replace (rather than risking a chunk-omission /
    // selective-update path that doesn't list the reclaimed slot — the
    // decoder MUST see the new slot value to stay in sync).
    let (force_full_v4, force_full_v1, reclaimed_v4, reclaimed_v1) =
        if let (Some(threshold), Some(seed_ref)) = (
            opts.stale_slot_threshold,
            seed_opt.as_mut().filter(|_| cross_frame_seeding),
        ) {
            let v4_recl = if let Some(seed_v4) = seed_ref.v4.as_mut() {
                reclaim_stale_slots_v4(
                    pixels,
                    width,
                    mode,
                    opts,
                    s,
                    Some(prev),
                    seed_v4,
                    roll.staleness(WhichCodebook::V4),
                    threshold,
                    opts.v4_entries as usize,
                )
            } else {
                Vec::new()
            };
            let v1_recl = if let Some(seed_v1) = seed_ref.v1.as_mut() {
                reclaim_stale_slots_v1(
                    pixels,
                    width,
                    mode,
                    opts,
                    s,
                    Some(prev),
                    seed_v1,
                    roll.staleness(WhichCodebook::V1),
                    threshold,
                    opts.v1_entries as usize,
                )
            } else {
                Vec::new()
            };
            (!v4_recl.is_empty(), !v1_recl.is_empty(), v4_recl, v1_recl)
        } else {
            (false, false, Vec::new(), Vec::new())
        };

    let plan = build_codebooks_and_decisions(
        pixels,
        width,
        mode,
        opts,
        s,
        Some(prev),
        true,
        seed_opt.as_ref(),
    )?;
    let StripPlanResult { v4_cb, v1_cb, mbs } = plan;

    // Determine which slots in each codebook are actually referenced by
    // the strip's macroblocks. Slots not referenced don't need to be
    // present in the decoder's codebook for this strip — and skipping
    // their emission saves wire bytes.
    let v4_used = used_v4_slots(&mbs);
    let v1_used = used_v1_slots(&mbs);

    let mut chunks = Vec::new();
    emit_codebook_chunks_inter(
        mode,
        &v4_cb,
        &v1_cb,
        opts,
        roll,
        selective_enabled,
        &v4_used,
        &v1_used,
        force_full_v4,
        force_full_v1,
        &mut chunks,
    );

    // Round-7: accumulate per-strip "used" masks into the
    // FrameStatsAccum so we can update staleness at the end of the
    // frame (not per-strip — a slot used in strip B but not strip A
    // shouldn't have its A-strip increment retroactively undone).
    // Also reset reclaimed slots immediately (they were just retrained
    // from current content).
    roll.reset_staleness(WhichCodebook::V4, &reclaimed_v4);
    roll.reset_staleness(WhichCodebook::V1, &reclaimed_v1);

    // Round-7 telemetry: accumulate per-strip stats (reclaimed-slot
    // count, forced-full count) and per-frame "any-strip-used" masks.
    // The free-function path passes None.
    if let Some(acc) = stats {
        acc.reclaimed_v4_slots += reclaimed_v4.len();
        acc.reclaimed_v1_slots += reclaimed_v1.len();
        if force_full_v4 {
            acc.forced_full_chunks += 1;
        }
        if force_full_v1 {
            acc.forced_full_chunks += 1;
        }
        for slot in 0..256 {
            acc.frame_v4_used[slot] |= v4_used[slot];
            acc.frame_v1_used[slot] |= v1_used[slot];
        }
    } else {
        // Stateless path: no per-frame aggregation, so update
        // staleness per-strip. The stateless path also doesn't
        // benefit from reclamation (cross_frame_seeding=false), so
        // staleness counters here are mostly cosmetic — but we keep
        // the logic consistent.
        roll.update_staleness(WhichCodebook::V4, &v4_used, opts.v4_entries as usize);
        roll.update_staleness(WhichCodebook::V1, &v1_used, opts.v1_entries as usize);
    }

    // Vector chunk 0x3100 (mixed inter with skip).
    let mut vec_payload = Vec::new();
    encode_inter_payload(&mbs, &mut vec_payload)?;
    let vec_chunk_size = (CHUNK_HEADER_SIZE + vec_payload.len()) as u16;
    chunks.extend_from_slice(&VECTOR_CHUNK_INTER.to_be_bytes());
    chunks.extend_from_slice(&vec_chunk_size.to_be_bytes());
    chunks.extend_from_slice(&vec_payload);

    finalise_strip(STRIP_ID_INTER, s, width, &chunks)
}

/// Round-7 telemetry accumulator: a thin per-frame counter struct that
/// the stateful encoder threads into each per-strip call so it can
/// report after-the-fact stats (reclaimed-slot counts, forced-full
/// chunks). Callers query this via [`CinepakEncoder::last_frame_stats`].
///
/// Also accumulates the per-frame "any strip referenced this slot?"
/// masks (`frame_v?_used`) so the post-frame staleness update is done
/// once per frame, not once per strip — otherwise slots would
/// over-count staleness on multi-strip frames (each unused strip
/// stripping the counter towards reclamation 1.5..N× faster than
/// intended).
#[derive(Clone, Copy)]
struct FrameStatsAccum {
    reclaimed_v4_slots: usize,
    reclaimed_v1_slots: usize,
    forced_full_chunks: usize,
    frame_v4_used: [bool; 256],
    frame_v1_used: [bool; 256],
}

impl Default for FrameStatsAccum {
    fn default() -> Self {
        Self {
            reclaimed_v4_slots: 0,
            reclaimed_v1_slots: 0,
            forced_full_chunks: 0,
            frame_v4_used: [false; 256],
            frame_v1_used: [false; 256],
        }
    }
}

/// Reclaim stale V4 slots in `seed` by overwriting their centroids
/// with high-residual sample sub-block vectors from the strip. Returns
/// the list of slot indices that were reclaimed (empty when no slot is
/// stale enough or the strip has no candidate vectors).
///
/// "High-residual" means: among non-skip sample vectors, the ones
/// whose distance to their nearest seed centroid is largest. We pick
/// distinct vectors (at least 2 codebook units of L1 separation) so
/// reclaiming N slots actually injects N distinct outliers, not N
/// copies of the same MB.
#[allow(clippy::too_many_arguments)]
fn reclaim_stale_slots_v4(
    pixels: &[u8],
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
    prev: Option<&CinepakFrame>,
    seed: &mut Codebook,
    staleness: &[u16; 256],
    threshold: u8,
    n: usize,
) -> Vec<u8> {
    // Identify stale slots (counter strictly greater-than-or-equal to
    // threshold ⇒ trigger reclamation; use >= so threshold = 1 means
    // "after 1 frame of staleness").
    let stale_slots: Vec<u8> = (0..n.min(256))
        .filter(|&slot| staleness[slot] > threshold as u16)
        .map(|slot| slot as u8)
        .collect();
    if stale_slots.is_empty() {
        return Vec::new();
    }
    // Sample the strip's non-skip V4 sub-block vectors.
    let mb_cols = (width / 4) as usize;
    let strip_h_mb = ((s.y_bottom - s.y_top) / 4) as usize;
    let mut samples: Vec<CodebookEntry> = Vec::with_capacity(strip_h_mb * mb_cols * 4);
    for r in 0..strip_h_mb {
        for c in 0..mb_cols {
            let py = s.y_top as usize + r * 4;
            let px = c * 4;
            let is_skip = if let Some(prev_f) = prev {
                let mse = mb_mse_against_prev(pixels, prev_f, px, py, width, mode);
                mse < opts.skip_threshold
            } else {
                false
            };
            if is_skip {
                continue;
            }
            samples.extend_from_slice(&sample_v4_block(pixels, width as usize, px, py, mode));
        }
    }
    reclaim_with_samples(&samples, mode, n, seed, &stale_slots, opts.luma_weight)
}

#[allow(clippy::too_many_arguments)]
fn reclaim_stale_slots_v1(
    pixels: &[u8],
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
    prev: Option<&CinepakFrame>,
    seed: &mut Codebook,
    staleness: &[u16; 256],
    threshold: u8,
    n: usize,
) -> Vec<u8> {
    let stale_slots: Vec<u8> = (0..n.min(256))
        .filter(|&slot| staleness[slot] > threshold as u16)
        .map(|slot| slot as u8)
        .collect();
    if stale_slots.is_empty() {
        return Vec::new();
    }
    let mb_cols = (width / 4) as usize;
    let strip_h_mb = ((s.y_bottom - s.y_top) / 4) as usize;
    let mut samples: Vec<CodebookEntry> = Vec::with_capacity(strip_h_mb * mb_cols);
    for r in 0..strip_h_mb {
        for c in 0..mb_cols {
            let py = s.y_top as usize + r * 4;
            let px = c * 4;
            let is_skip = if let Some(prev_f) = prev {
                let mse = mb_mse_against_prev(pixels, prev_f, px, py, width, mode);
                mse < opts.skip_threshold
            } else {
                false
            };
            if is_skip {
                continue;
            }
            samples.push(sample_v1_block(pixels, width as usize, px, py, mode));
        }
    }
    reclaim_with_samples(&samples, mode, n, seed, &stale_slots, opts.luma_weight)
}

/// Common reclamation kernel: given non-skip `samples`, the codebook
/// `n`, the seed codebook to patch, and the list of `stale_slots` to
/// reclaim, picks one distinct high-residual sample per stale slot and
/// overwrites the seed centroid with it. Returns the list of slots
/// that were actually reclaimed (a slot is skipped if no
/// sufficiently-distinct outlier remains).
fn reclaim_with_samples(
    samples: &[CodebookEntry],
    mode: PixelMode,
    n: usize,
    seed: &mut Codebook,
    stale_slots: &[u8],
    luma_weight: u8,
) -> Vec<u8> {
    if samples.is_empty() || n == 0 {
        return Vec::new();
    }
    // Score every sample by its distance to its nearest seed centroid.
    // Reclaim the highest-residual samples first; within that ranking,
    // skip samples too close to an already-picked outlier (so two
    // reclaimed slots don't end up with the same centroid).
    let mut scored: Vec<(i64, usize)> = (0..samples.len())
        .map(|i| {
            let (_, d) = nearest(&samples[i], seed, n, mode, luma_weight);
            (d, i)
        })
        .collect();
    scored.sort_by_key(|x| std::cmp::Reverse(x.0)); // descending residual
    let min_separation: u32 = 4; // L1 units
    let mut chosen_indices: Vec<usize> = Vec::with_capacity(stale_slots.len());
    for (_, idx) in scored {
        if chosen_indices.len() >= stale_slots.len() {
            break;
        }
        let candidate = &samples[idx];
        let too_close = chosen_indices.iter().any(|&j| {
            entry_l1_distance(candidate, &samples[j], mode, luma_weight) < min_separation
        });
        if !too_close {
            chosen_indices.push(idx);
        }
    }
    if chosen_indices.is_empty() {
        return Vec::new();
    }
    let mut reclaimed: Vec<u8> = Vec::with_capacity(chosen_indices.len());
    for (slot_idx, sample_idx) in chosen_indices.iter().enumerate() {
        let slot = stale_slots[slot_idx];
        seed.entries[slot as usize] = samples[*sample_idx];
        reclaimed.push(slot);
    }
    reclaimed
}

/// Per-strip codebook + per-MB decision result.
struct StripPlanResult {
    v4_cb: Codebook,
    v1_cb: Codebook,
    mbs: Vec<Mb>,
}

/// Optional seed codebooks for cross-frame median-cut warm-start
/// (round 5). When present, each codebook's centroids are used as the
/// initial assignments for one Lloyd-style refinement pass on the
/// freshly-sampled vectors. Slots whose seed centroid attracts no
/// vectors retain the seed value (so the decoder's existing entry at
/// that slot stays valid — important for selective-update / chunk
/// omission).
#[derive(Clone, Default)]
struct SeedCodebooks {
    v4: Option<Codebook>,
    v1: Option<Codebook>,
}

/// Sample the strip's macroblocks, build V1 + V4 codebooks via
/// median-cut, and decide V1 / V4 / Skip per MB.
///
/// On `is_inter == true` and `prev = Some(...)`, MBs whose per-pixel
/// MSE against the prev frame is below `opts.skip_threshold` are coded
/// as `Mb::Skip`.
///
/// `seed`, when `Some`, supplies prior-frame codebooks to warm-start
/// the median-cut quantiser (cross-frame codebook persistence).
#[allow(clippy::too_many_arguments)]
fn build_codebooks_and_decisions(
    pixels: &[u8],
    width: u32,
    mode: PixelMode,
    opts: &EncoderOptions,
    s: &StripPlan,
    prev: Option<&CinepakFrame>,
    is_inter: bool,
    seed: Option<&SeedCodebooks>,
) -> Result<StripPlanResult> {
    let mb_cols = (width / 4) as usize;
    let strip_h_mb = ((s.y_bottom - s.y_top) / 4) as usize;
    let mb_count = strip_h_mb * mb_cols;

    let mut v4_vectors: Vec<CodebookEntry> = Vec::with_capacity(mb_count * 4);
    let mut v1_vectors: Vec<CodebookEntry> = Vec::with_capacity(mb_count);
    let mut mb_v4: Vec<[CodebookEntry; 4]> = Vec::with_capacity(mb_count);
    let mut mb_v1: Vec<CodebookEntry> = Vec::with_capacity(mb_count);
    // Track which MBs are SKIP candidates so we don't pollute the
    // codebook training set with their data (they don't need codebook
    // entries).
    let mut skip_mask: Vec<bool> = vec![false; mb_count];

    for r in 0..strip_h_mb {
        for c in 0..mb_cols {
            let py = s.y_top as usize + r * 4;
            let px = c * 4;
            let v4 = sample_v4_block(pixels, width as usize, px, py, mode);
            let v1 = sample_v1_block(pixels, width as usize, px, py, mode);

            let is_skip = if is_inter {
                if let Some(prev_f) = prev {
                    let mse = mb_mse_against_prev(pixels, prev_f, px, py, width, mode);
                    mse < opts.skip_threshold
                } else {
                    false
                }
            } else {
                false
            };

            let mb_idx = r * mb_cols + c;
            skip_mask[mb_idx] = is_skip;

            if !is_skip {
                v4_vectors.extend_from_slice(&v4);
                v1_vectors.push(v1);
            }
            mb_v4.push(v4);
            mb_v1.push(v1);
        }
    }

    // Build codebooks via median-cut on the non-skipped vectors. If
    // every MB is a skip, leave codebooks at default (zero) — fine,
    // they won't be referenced.
    //
    // When a seed codebook (matching N + mode) is supplied, warm-start
    // median-cut with `opts.lloyd_max_iter` Lloyd-style refinement
    // passes anchored to the seed centroids (round-6 tighter Lloyd
    // refinement); this preserves slot identity across frames and
    // amplifies selective-update / chunk-omission wins downstream.
    let v4_seed = seed.and_then(|s| s.v4.as_ref());
    let v1_seed = seed.and_then(|s| s.v1.as_ref());
    let mut v4_codebook = median_cut_seeded(
        &v4_vectors,
        opts.v4_entries as usize,
        mode,
        v4_seed,
        opts.lloyd_max_iter,
        opts.lloyd_eps,
        opts.luma_weight,
        opts.kmeans_pp_init,
        opts.kmeans_pp_lloyd_iter,
    );
    let mut v1_codebook = median_cut_seeded(
        &v1_vectors,
        opts.v1_entries as usize,
        mode,
        v1_seed,
        opts.lloyd_max_iter,
        opts.lloyd_eps,
        opts.luma_weight,
        opts.kmeans_pp_init,
        opts.kmeans_pp_lloyd_iter,
    );
    // Round 4 Lever E: LBG split refinement (Linde-Buzo-Gray 1980).
    // After the median-cut + Lloyd warm-build, iteratively split the
    // highest-distortion populated slot into the lowest-population slot
    // and run one extra Lloyd assignment + recentroid pass — until no
    // such split improves total SSE.
    if opts.lbg_max_passes > 0 {
        lbg_refine_codebook(
            &mut v4_codebook,
            &v4_vectors,
            opts.v4_entries as usize,
            mode,
            opts.lbg_max_passes,
            opts.luma_weight,
        );
        lbg_refine_codebook(
            &mut v1_codebook,
            &v1_vectors,
            opts.v1_entries as usize,
            mode,
            opts.lbg_max_passes,
            opts.luma_weight,
        );
    }

    // Classify each MB.
    let mut mbs = classify_mbs(
        &mb_v4,
        &mb_v1,
        &skip_mask,
        &v4_codebook,
        &v1_codebook,
        opts,
        mode,
    );

    // Round 6 Lever H: post-classification Lloyd polish. Re-train each
    // *used* codebook slot from only the actually-selected vectors (the
    // RDO step routes some V4-trained vectors to V1 and vice-versa, so
    // the LBG-trained centroids are not the means of the slot's actual
    // selected member set), then re-classify and repeat. Slot identity
    // is preserved (unused slots stay byte-identical to the LBG output)
    // so cross-frame persistence and selective-update / chunk-omission
    // wins are unaffected.
    if opts.pcl_max_iter > 0 {
        for _iter in 0..opts.pcl_max_iter {
            let changed = post_classification_polish(
                &mut v4_codebook,
                &mut v1_codebook,
                &mb_v4,
                &mb_v1,
                &mbs,
                mode,
            );
            if !changed {
                break;
            }
            mbs = classify_mbs(
                &mb_v4,
                &mb_v1,
                &skip_mask,
                &v4_codebook,
                &v1_codebook,
                opts,
                mode,
            );
        }
    }

    Ok(StripPlanResult {
        v4_cb: v4_codebook,
        v1_cb: v1_codebook,
        mbs,
    })
}

/// Per-MB V4/V1/Skip classification (Round-3 Lagrangian RDO selection,
/// extracted from `build_codebooks_and_decisions` so the round-6
/// post-classification Lloyd polish can re-run it after each centroid
/// update). The per-MB bit-cost delta is `R_v4 - R_v1 = 24` bits (V4: 4
/// index bytes vs V1: 1 index byte; flag-bit cost is identical). Picks
/// V1 when its pixel-domain Y SSE excess is `≤ lambda · 24`; otherwise
/// V4. With `opts.rdo_lambda = None` falls back to the legacy round-2
/// behaviour: pick whichever flavour's codebook-distance metric is
/// smaller, tiebreak toward V1 (smaller wire footprint).
fn classify_mbs(
    mb_v4: &[[CodebookEntry; 4]],
    mb_v1: &[CodebookEntry],
    skip_mask: &[bool],
    v4_codebook: &Codebook,
    v1_codebook: &Codebook,
    opts: &EncoderOptions,
    mode: PixelMode,
) -> Vec<Mb> {
    const RDO_DELTA_BITS: f32 = 24.0;
    let mb_count = mb_v4.len();
    let mut mbs: Vec<Mb> = Vec::with_capacity(mb_count);
    for i in 0..mb_count {
        if skip_mask[i] {
            mbs.push(Mb::Skip);
            continue;
        }
        let (v4_idx, v4_err) = pick_v4(
            &mb_v4[i],
            v4_codebook,
            opts.v4_entries as usize,
            opts.luma_weight,
        );
        let (v1_idx, v1_err) = pick_v1(
            &mb_v1[i],
            v1_codebook,
            opts.v1_entries as usize,
            mode,
            opts.luma_weight,
        );
        let pick_v1_flag = if let Some(lambda) = opts.rdo_lambda {
            let (d_v4_pix, d_v1_pix) =
                rdo_pixel_y_sse(&mb_v4[i], &v4_idx, v4_codebook, v1_idx, v1_codebook);
            let lhs = (d_v1_pix - d_v4_pix) as f32;
            let rhs = lambda * RDO_DELTA_BITS;
            lhs <= rhs
        } else {
            v1_err <= v4_err
        };
        if pick_v1_flag {
            mbs.push(Mb::V1(v1_idx));
        } else {
            mbs.push(Mb::V4(v4_idx));
        }
    }
    mbs
}

/// Round 6 Lever H: re-train each *used* codebook slot from the
/// actually-selected vectors. For V4: slot `s` is updated to the mean
/// of all sub-block vectors `mb_v4[i][sub]` such that `mbs[i] ==
/// V4(idx)` and `idx[sub] == s`. For V1: slot `s` is updated to the
/// mean of all `mb_v1[i]` such that `mbs[i] == V1(s)`. Returns `true`
/// iff at least one entry actually changed bytes (so the caller can
/// early-stop the Lloyd polish loop).
///
/// Slots not referenced by any MB are left byte-identical to their
/// pre-call value (slot identity is preserved across the polish, so
/// cross-frame persistence and selective-update / chunk-omission wins
/// on inter strips are unaffected).
fn post_classification_polish(
    v4_codebook: &mut Codebook,
    v1_codebook: &mut Codebook,
    mb_v4: &[[CodebookEntry; 4]],
    mb_v1: &[CodebookEntry],
    mbs: &[Mb],
    mode: PixelMode,
) -> bool {
    // Per-slot accumulators: cluster member vectors so we can compute
    // exact centroids (matches the round-4 LBG / round-5 Lloyd centroid
    // semantics — integer mean per dim, with chroma zeroed in Gray8).
    let mut v4_clusters: Vec<Vec<CodebookEntry>> = (0..256).map(|_| Vec::new()).collect();
    let mut v1_clusters: Vec<Vec<CodebookEntry>> = (0..256).map(|_| Vec::new()).collect();
    for (i, mb) in mbs.iter().enumerate() {
        match mb {
            Mb::V4(idx) => {
                for sub in 0..4 {
                    v4_clusters[idx[sub] as usize].push(mb_v4[i][sub]);
                }
            }
            Mb::V1(idx) => {
                v1_clusters[*idx as usize].push(mb_v1[i]);
            }
            Mb::Skip => {}
        }
    }
    let mut changed = false;
    for slot in 0..256 {
        if !v4_clusters[slot].is_empty() {
            let new_centroid = centroid(&v4_clusters[slot], mode);
            if v4_codebook.entries[slot] != new_centroid {
                v4_codebook.entries[slot] = new_centroid;
                changed = true;
            }
        }
        if !v1_clusters[slot].is_empty() {
            let new_centroid = centroid(&v1_clusters[slot], mode);
            if v1_codebook.entries[slot] != new_centroid {
                v1_codebook.entries[slot] = new_centroid;
                changed = true;
            }
        }
    }
    changed
}

fn emit_codebook_chunks_full(
    mode: PixelMode,
    v4_cb: &Codebook,
    v1_cb: &Codebook,
    opts: &EncoderOptions,
    out: &mut Vec<u8>,
) {
    let v4_kind = CodebookChunkKind {
        which: WhichCodebook::V4,
        style: UpdateStyle::Full,
        mode,
    };
    let v1_kind = CodebookChunkKind {
        which: WhichCodebook::V1,
        style: UpdateStyle::Full,
        mode,
    };
    encode_full_chunk(v4_kind, v4_cb, opts.v4_entries as usize, out);
    encode_full_chunk(v1_kind, v1_cb, opts.v1_entries as usize, out);
}

/// Emit codebook chunks for an inter strip, choosing between
/// full-replace, selective-update, and "no chunk at all" (= inherit
/// previous strip's codebook) per codebook flavour. Updates `roll`'s
/// state to reflect what the decoder will hold after applying these
/// chunks, since subsequent strips (and frames) inherit from there.
///
/// Wire-size budget per chunk:
///
/// - `full-replace`: `4 + N × entry_size`.
/// - `selective`: `4 + Σ_g (4 + entry_size × popcount(flag_g))`,
///   where `entry_size` is `6` (Yuv12) or `4` (Gray8).
/// - `none`: 0 (legal when the codebook is identical to the inherited
///   one and all referenced slots are already populated correctly).
#[allow(clippy::too_many_arguments)]
fn emit_codebook_chunks_inter(
    mode: PixelMode,
    v4_cb: &Codebook,
    v1_cb: &Codebook,
    opts: &EncoderOptions,
    roll: &mut RollingCodebooks,
    selective_enabled: bool,
    v4_used: &[bool; 256],
    v1_used: &[bool; 256],
    force_full_v4: bool,
    force_full_v1: bool,
    out: &mut Vec<u8>,
) {
    emit_codebook_chunk_one(
        mode,
        WhichCodebook::V4,
        v4_cb,
        opts.v4_entries as usize,
        roll,
        selective_enabled,
        v4_used,
        force_full_v4,
        out,
    );
    emit_codebook_chunk_one(
        mode,
        WhichCodebook::V1,
        v1_cb,
        opts.v1_entries as usize,
        roll,
        selective_enabled,
        v1_used,
        force_full_v1,
        out,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_codebook_chunk_one(
    mode: PixelMode,
    which: WhichCodebook,
    cb: &Codebook,
    n: usize,
    roll: &mut RollingCodebooks,
    selective_enabled: bool,
    used: &[bool; 256],
    force_full: bool,
    out: &mut Vec<u8>,
) {
    let entry_size = mode.entry_size();
    let prev = roll.get(which);

    // Compute "differing-slot" mask vs prev codebook (only meaningful
    // if a prev exists with the same N).
    let prev_matches = match prev {
        Some((prev_cb, prev_n)) if *prev_n == n => Some(prev_cb),
        _ => None,
    };

    // 1) Determine which slots actually need to differ from the
    //    decoder's current state. Slots not used by any MB in this
    //    strip don't need to be "correct" — their decoder-side value
    //    is irrelevant.
    let mut need_update: Vec<u8> = Vec::new();
    for slot in 0..n {
        if !used[slot] {
            continue;
        }
        let differs = match prev_matches {
            Some(prev_cb) => prev_cb.entries[slot] != cb.entries[slot],
            None => true,
        };
        if differs {
            need_update.push(slot as u8);
        }
    }

    // 2) Decide chunk format by wire-size cost.
    let full_size = CHUNK_HEADER_SIZE + n * entry_size;
    let none_size = 0usize;
    let selective_size = if need_update.is_empty() {
        // Empty selective-update would still cost a 4-byte header +
        // 4-byte zero flag word = 8 bytes. The "none" form (omit chunk
        // entirely) wins.
        usize::MAX
    } else {
        let max_slot = *need_update.last().unwrap() as usize;
        let groups = (max_slot / 32) + 1;
        CHUNK_HEADER_SIZE + groups * 4 + need_update.len() * entry_size
    };

    // Conditions: emitting "none" only legal if the prev codebook
    // satisfies all referenced slots already (i.e. no need_update
    // entries). For first-strip-of-first-frame (no prev), full is the
    // only option.
    let can_emit_none = prev_matches.is_some() && need_update.is_empty();

    let kind_full = CodebookChunkKind {
        which,
        style: UpdateStyle::Full,
        mode,
    };
    let kind_sel = CodebookChunkKind {
        which,
        style: UpdateStyle::Selective,
        mode,
    };

    if force_full {
        // Round-7: a stale slot was reclaimed for this codebook
        // flavour, so we MUST emit the full codebook so the decoder
        // sees the reclaimed slot value (selective + listing the
        // reclaimed slot would also work, but full-replace is simpler
        // and the wire-size delta is small for a one-shot reclaim).
        encode_full_chunk(kind_full, cb, n, out);
        roll.set(which, cb.clone(), n);
    } else if can_emit_none && none_size <= full_size {
        // Omit chunk entirely. Decoder inherits prev codebook (spec §5).
        // Roll-state update: codebook stays exactly as `prev` — the
        // slots not referenced by this strip's MBs may differ between
        // `cb` and `prev`, so we MUST record the decoder's actual
        // view (= `prev`) rather than our freshly-trained `cb` to
        // keep cross-strip codebook tracking accurate.
        let prev_cb = prev_matches.unwrap().clone();
        roll.set(which, prev_cb, n);
    } else if let Some(prev_cb) = prev_matches
        .filter(|_| selective_enabled && !need_update.is_empty() && selective_size < full_size)
    {
        encode_selective_chunk(kind_sel, cb, &need_update, out);
        // After selective-update, the decoder's codebook = prev's
        // codebook with the listed slots replaced. Build that view.
        let mut new_cb = prev_cb.clone();
        for &slot in &need_update {
            new_cb.entries[slot as usize] = cb.entries[slot as usize];
        }
        roll.set(which, new_cb, n);
    } else {
        encode_full_chunk(kind_full, cb, n, out);
        roll.set(which, cb.clone(), n);
    }
}

/// Bitmask of V4 codebook slots referenced by any V4 macroblock.
fn used_v4_slots(mbs: &[Mb]) -> [bool; 256] {
    let mut used = [false; 256];
    for mb in mbs {
        if let Mb::V4(idx) = mb {
            for &i in idx {
                used[i as usize] = true;
            }
        }
    }
    used
}

/// Bitmask of V1 codebook slots referenced by any V1 macroblock.
fn used_v1_slots(mbs: &[Mb]) -> [bool; 256] {
    let mut used = [false; 256];
    for mb in mbs {
        if let Mb::V1(idx) = mb {
            used[*idx as usize] = true;
        }
    }
    used
}

/// Tracks the V4 and V1 codebook the decoder will hold at the start of
/// the next strip. Updated after emitting each strip's codebook chunks.
///
/// Round-7 additions: also tracks a per-slot **staleness counter** for
/// each codebook flavour — incremented at the end of every inter strip
/// for any slot not referenced in that strip, reset to zero for any
/// slot that *was* referenced. The encoder reads these counters before
/// the next strip's codebook training step to decide whether any slot
/// has gone unreferenced long enough that it should be reclaimed
/// (re-seeded from a high-residual MB) rather than left frozen forever
/// via cross-frame persistence.
#[derive(Clone)]
struct RollingCodebooks {
    v4: Option<(Codebook, usize)>,
    v1: Option<(Codebook, usize)>,
    /// Round-7 per-slot staleness: number of consecutive inter strips
    /// since the slot was last referenced by an MB. Indexed `[slot]`,
    /// `0..256`. Reset when a slot is reseeded or when an intra frame
    /// resets the rolling state. Saturates at `u16::MAX`.
    v4_stale: [u16; 256],
    v1_stale: [u16; 256],
}

impl Default for RollingCodebooks {
    fn default() -> Self {
        Self {
            v4: None,
            v1: None,
            v4_stale: [0u16; 256],
            v1_stale: [0u16; 256],
        }
    }
}

impl RollingCodebooks {
    fn get(&self, which: WhichCodebook) -> Option<&(Codebook, usize)> {
        match which {
            WhichCodebook::V4 => self.v4.as_ref(),
            WhichCodebook::V1 => self.v1.as_ref(),
        }
    }

    fn set(&mut self, which: WhichCodebook, cb: Codebook, n: usize) {
        match which {
            WhichCodebook::V4 => self.v4 = Some((cb, n)),
            WhichCodebook::V1 => self.v1 = Some((cb, n)),
        }
    }

    /// Bump the per-slot staleness counters for the codebook flavour
    /// `which`: increment each slot in `0..n` not present in `used`,
    /// reset to zero each slot that *is* present. Slots `≥ n` are
    /// untouched. Saturates at `u16::MAX`.
    fn update_staleness(&mut self, which: WhichCodebook, used: &[bool; 256], n: usize) {
        let stale = match which {
            WhichCodebook::V4 => &mut self.v4_stale,
            WhichCodebook::V1 => &mut self.v1_stale,
        };
        for slot in 0..n.min(256) {
            if used[slot] {
                stale[slot] = 0;
            } else {
                stale[slot] = stale[slot].saturating_add(1);
            }
        }
    }

    /// Read the current per-slot staleness counters for `which`.
    fn staleness(&self, which: WhichCodebook) -> &[u16; 256] {
        match which {
            WhichCodebook::V4 => &self.v4_stale,
            WhichCodebook::V1 => &self.v1_stale,
        }
    }

    /// Reset the per-slot staleness counters for `which` to all zero.
    /// Called after slot reclamation (those slots have just been
    /// reseeded) and on intra frames (rolling state reset).
    fn reset_staleness(&mut self, which: WhichCodebook, slots: &[u8]) {
        let stale = match which {
            WhichCodebook::V4 => &mut self.v4_stale,
            WhichCodebook::V1 => &mut self.v1_stale,
        };
        for &s in slots {
            stale[s as usize] = 0;
        }
    }
}

fn finalise_strip(strip_id: u16, s: &StripPlan, width: u32, chunks: &[u8]) -> Result<Vec<u8>> {
    let strip_size_usize = STRIP_HEADER_SIZE + chunks.len();
    if strip_size_usize > u16::MAX as usize {
        return Err(CinepakError::other(format!(
            "encoder: strip exceeds 16-bit strip_size budget ({strip_size_usize} > 65535); reduce codebook size or split into more strips"
        )));
    }
    let strip_size = strip_size_usize as u16;
    let raw = RawStripHeader {
        strip_id,
        strip_size,
        y_top: s.y_top as u16,
        x_top: 0,
        y_bottom: s.y_bottom as u16,
        x_bottom: width as u16,
    };
    let mut out = Vec::with_capacity(strip_size as usize);
    let mut hdr_buf = [0u8; STRIP_HEADER_SIZE];
    raw.encode(&mut hdr_buf);
    out.extend_from_slice(&hdr_buf);
    out.extend_from_slice(chunks);
    Ok(out)
}

fn assemble_frame(width: u32, height: u32, strips: &[Vec<u8>]) -> Result<Vec<u8>> {
    let strips_total: usize = strips.iter().map(|s| s.len()).sum();
    let frame_length_usize = FRAME_HEADER_SIZE + strips_total;
    if frame_length_usize > 0x00ff_ffff {
        return Err(CinepakError::other(format!(
            "encoder: frame_length exceeds 24-bit budget ({frame_length_usize} > 16777215)"
        )));
    }
    let frame_length = frame_length_usize as u32;
    let frame_hdr = FrameHeader {
        flags: 0,
        frame_length,
        width: width as u16,
        height: height as u16,
        strip_count: strips.len() as u16,
    };
    let mut out = Vec::with_capacity(frame_length as usize);
    let mut fhdr_buf = [0u8; FRAME_HEADER_SIZE];
    frame_hdr.encode(&mut fhdr_buf);
    out.extend_from_slice(&fhdr_buf);
    for s in strips {
        out.extend_from_slice(s);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_dims(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || width % 4 != 0 || height % 4 != 0 {
        return Err(CinepakError::other(format!(
            "encoder: dims must be > 0 and multiples of 4; got {width}x{height}"
        )));
    }
    Ok(())
}

fn validate_opts(opts: &EncoderOptions) -> Result<()> {
    if !(1..=256).contains(&(opts.v4_entries as u32))
        || !(1..=256).contains(&(opts.v1_entries as u32))
    {
        return Err(CinepakError::other(
            "encoder: v4_entries / v1_entries must be in 1..=256",
        ));
    }
    if opts.strip_count == 0 {
        return Err(CinepakError::other("encoder: strip_count must be ≥ 1"));
    }
    if !(opts.skip_threshold.is_finite() && opts.skip_threshold >= 0.0) {
        return Err(CinepakError::other(
            "encoder: skip_threshold must be a finite non-negative float",
        ));
    }
    Ok(())
}

fn validate_input_size(pixels: &[u8], width: u32, height: u32, mode: PixelMode) -> Result<()> {
    let bpp = match mode {
        PixelMode::Yuv12 => 3,
        PixelMode::Gray8 => 1,
    };
    let expected = (width as usize) * (height as usize) * bpp;
    if pixels.len() < expected {
        return Err(CinepakError::other(format!(
            "encoder: input buffer {} bytes < expected {expected}",
            pixels.len()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// RGB → YUV sampling
// ---------------------------------------------------------------------------

/// Forward of the spec's inverse matrix (yuv → rgb in
/// `04-yuv-rgb-matrix.md` §3):
///
/// ```text
///   R = Y + 2V        (1)
///   G = Y - U/2 - V   (2)
///   B = Y + 2U        (3)
/// ```
///
/// Solve (1)..(3) simultaneously for `(Y, U, V)`:
///
/// ```text
///   V = (R - Y) / 2          from (1)
///   U = (B - Y) / 2          from (3)
///   substitute in (2):
///     G = Y - (B - Y)/4 - (R - Y)/2
///       = Y - B/4 + Y/4 - R/2 + Y/2
///       = (7/4) Y - B/4 - R/2
///   ⇒  Y = (4G + B + 2R) / 7
/// ```
///
/// This formula round-trips the primary fixtures `T1a..T1e` exactly
/// (per spec/02 §6.1 table): pure red `(255, 0, 0)` gives `(73, -36,
/// 91)` (decoder: `Y=73, U=-36, V=91 → R=255, G=0, B=1`); pure green
/// `(0, 255, 0)` gives `(145, -73, -73)`, decoder `(0, 254, 0)` —
/// matching `T1c`'s spec-quoted `M2` fixture. Pure blue `(0, 0, 255)`
/// gives `(36, +110, -18)` ≈ wire fixture `T1b`'s `(108, -19)`.
#[inline]
fn rgb_to_yuv(r: u8, g: u8, b: u8) -> (u8, i8, i8) {
    let r = r as i32;
    let g = g as i32;
    let b = b as i32;
    // Y = (4G + B + 2R) / 7, with rounding-to-nearest.
    let y = (2 * r + 4 * g + b + 3) / 7;
    let y = y.clamp(0, 255);
    let u = (b - y) / 2;
    let v = (r - y) / 2;
    (y as u8, u.clamp(-128, 127) as i8, v.clamp(-128, 127) as i8)
}

/// Sample one V4 macroblock's four 2×2 sub-blocks. Returns four
/// codebook-entry values, one per sub-block (TL, TR, BL, BR).
fn sample_v4_block(
    pixels: &[u8],
    pixel_stride_pixels: usize,
    px: usize,
    py: usize,
    mode: PixelMode,
) -> [CodebookEntry; 4] {
    let mut out = [CodebookEntry::default(); 4];
    for sub_idx in 0..4 {
        let sub_row = sub_idx / 2;
        let sub_col = sub_idx % 2;
        // Each entry's Yi is one of the four pixels in the 2×2 sub-block.
        // Layout: Y0 at (0,0), Y1 at (0,1), Y2 at (1,0), Y3 at (1,1).
        let mut ys = [0u8; 4];
        let (mut acc_r, mut acc_g, mut acc_b) = (0i32, 0i32, 0i32);
        for pixel_idx in 0..4 {
            let dy = pixel_idx / 2;
            let dx = pixel_idx % 2;
            let row = py + sub_row * 2 + dy;
            let col = px + sub_col * 2 + dx;
            match mode {
                PixelMode::Yuv12 => {
                    let off = row * pixel_stride_pixels * 3 + col * 3;
                    let (r, g, b) = (pixels[off], pixels[off + 1], pixels[off + 2]);
                    let (y, _, _) = rgb_to_yuv(r, g, b);
                    ys[pixel_idx] = y;
                    acc_r += i32::from(r);
                    acc_g += i32::from(g);
                    acc_b += i32::from(b);
                }
                PixelMode::Gray8 => {
                    let off = row * pixel_stride_pixels + col;
                    ys[pixel_idx] = pixels[off];
                }
            }
        }
        let (u, v) = match mode {
            PixelMode::Yuv12 => {
                // Average chroma across the 2×2 sub-block.
                let r = (acc_r / 4) as u8;
                let g = (acc_g / 4) as u8;
                let b = (acc_b / 4) as u8;
                let (_, u, v) = rgb_to_yuv(r, g, b);
                (u, v)
            }
            PixelMode::Gray8 => (0, 0),
        };
        out[sub_idx] = CodebookEntry {
            y0: ys[0],
            y1: ys[1],
            y2: ys[2],
            y3: ys[3],
            u,
            v,
        };
    }
    out
}

/// Sample one V1 macroblock as a single codebook entry. Each `Yi`
/// represents the average luminance over one 2×2 quadrant of the 4×4
/// macroblock.
fn sample_v1_block(
    pixels: &[u8],
    pixel_stride_pixels: usize,
    px: usize,
    py: usize,
    mode: PixelMode,
) -> CodebookEntry {
    let mut quad_ys = [0u8; 4];
    let (mut acc_r, mut acc_g, mut acc_b) = (0i32, 0i32, 0i32);
    let mut total_pixels = 0i32;
    for quad_idx in 0..4 {
        let quad_row = quad_idx / 2;
        let quad_col = quad_idx % 2;
        let mut sum_y = 0i32;
        for dy in 0..2 {
            for dx in 0..2 {
                let row = py + quad_row * 2 + dy;
                let col = px + quad_col * 2 + dx;
                match mode {
                    PixelMode::Yuv12 => {
                        let off = row * pixel_stride_pixels * 3 + col * 3;
                        let (r, g, b) = (pixels[off], pixels[off + 1], pixels[off + 2]);
                        let (y, _, _) = rgb_to_yuv(r, g, b);
                        sum_y += i32::from(y);
                        acc_r += i32::from(r);
                        acc_g += i32::from(g);
                        acc_b += i32::from(b);
                        total_pixels += 1;
                    }
                    PixelMode::Gray8 => {
                        let off = row * pixel_stride_pixels + col;
                        sum_y += i32::from(pixels[off]);
                        total_pixels += 1;
                    }
                }
            }
        }
        quad_ys[quad_idx] = (sum_y / 4) as u8;
    }
    let (u, v) = match mode {
        PixelMode::Yuv12 => {
            let r = (acc_r / total_pixels) as u8;
            let g = (acc_g / total_pixels) as u8;
            let b = (acc_b / total_pixels) as u8;
            let (_, u, v) = rgb_to_yuv(r, g, b);
            (u, v)
        }
        PixelMode::Gray8 => (0, 0),
    };
    CodebookEntry {
        y0: quad_ys[0],
        y1: quad_ys[1],
        y2: quad_ys[2],
        y3: quad_ys[3],
        u,
        v,
    }
}

// ---------------------------------------------------------------------------
// Inter-frame skip detection
// ---------------------------------------------------------------------------

/// Per-pixel mean-squared error between the source 4×4 block at
/// `(px, py)` in the input RGB buffer and the same-position 4×4 block
/// in `prev`. Used to decide SKIP eligibility for inter macroblocks.
///
/// For `Yuv12` mode, error is computed over the three RGB channels —
/// not the encoded YUV space — because the SKIP decision is about the
/// **output** pixel buffer that will be visible to the decoder. For
/// `Gray8`, error is over the single luminance channel.
fn mb_mse_against_prev(
    pixels: &[u8],
    prev: &CinepakFrame,
    px: usize,
    py: usize,
    width: u32,
    mode: PixelMode,
) -> f32 {
    let prev_stride = prev.planes[0].stride;
    let prev_data = &prev.planes[0].data;
    let mut sum_sq: f32 = 0.0;
    let mut n: f32 = 0.0;
    match mode {
        PixelMode::Yuv12 => {
            for dy in 0..4 {
                for dx in 0..4 {
                    let row = py + dy;
                    let col = px + dx;
                    let src_off = row * (width as usize) * 3 + col * 3;
                    let prev_off = row * prev_stride + col * 3;
                    for ch in 0..3 {
                        let s = pixels[src_off + ch] as f32;
                        let p = prev_data[prev_off + ch] as f32;
                        let d = s - p;
                        sum_sq += d * d;
                        n += 1.0;
                    }
                }
            }
        }
        PixelMode::Gray8 => {
            for dy in 0..4 {
                for dx in 0..4 {
                    let row = py + dy;
                    let col = px + dx;
                    let src_off = row * (width as usize) + col;
                    let prev_off = row * prev_stride + col;
                    let s = pixels[src_off] as f32;
                    let p = prev_data[prev_off] as f32;
                    let d = s - p;
                    sum_sq += d * d;
                    n += 1.0;
                }
            }
        }
    }
    if n > 0.0 {
        sum_sq / n
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Median-cut codebook quantiser
// ---------------------------------------------------------------------------

/// Build a codebook of up to `n` entries from the population
/// `vectors`, optionally seeded with prior-frame centroids.
///
/// When `seed` is `None`, this is the round-2 cold-start median-cut
/// quantiser. When `seed = Some(prev_cb)` and `prev_cb` has at least
/// `n` populated entries, each input vector is assigned to the slot of
/// its nearest seed centroid (Lloyd iteration); the new codebook's
/// slot `i` is the centroid of the vectors that landed in the seed's
/// slot `i`. Slots that attract no vectors retain the seed centroid —
/// this is the round-5 cross-frame persistence behaviour: a codebook
/// slot that was correct last frame stays byte-identical this frame
/// even if no current MB happens to need it, so the decoder's prior
/// state for that slot remains valid for chunk-omission / selective-
/// update.
///
/// `max_iter` controls the round-6 tighter Lloyd refinement loop: each
/// iteration reassigns vectors against the *current* (not seed)
/// centroids and recomputes them, with early stop when the largest
/// per-slot Manhattan drift falls to `≤ eps`. `max_iter = 0` falls
/// back to the cold-start median-cut (no warm-start), `max_iter = 1`
/// reproduces the round-5 single-pass behaviour. Slot identity is
/// preserved across iterations because each step reassigns to the
/// nearest *current-iteration* centroid (which started as the seed),
/// so slots that began as seed-i remain "the seed-i lineage" and the
/// chunk-omission / selective-update wins downstream are unaffected.
#[allow(clippy::too_many_arguments)]
fn median_cut_seeded(
    vectors: &[CodebookEntry],
    n: usize,
    mode: PixelMode,
    seed: Option<&Codebook>,
    max_iter: u8,
    eps: u32,
    luma_weight: u8,
    kmeans_pp_init: bool,
    kmeans_pp_lloyd_iter: u8,
) -> Codebook {
    if max_iter == 0 {
        // Lloyd disabled — cold-start median-cut (ignores seed).
        return median_cut(vectors, n, mode, luma_weight);
    }
    if let Some(prev) = seed {
        if !vectors.is_empty() && n > 0 {
            // Iterative Lloyd refinement starting from the seed
            // centroids. We keep the seed's slot order so that
            // cross-frame slot-identity is preserved (slot `i` of the
            // returned codebook is the descendant of the seed's slot
            // `i`; slots that attract no vectors at any iteration
            // retain the seed's slot `i` byte-identical).
            let mut cb = prev.clone();
            for _iter in 0..max_iter {
                let mut clusters: Vec<Vec<CodebookEntry>> = vec![Vec::new(); n];
                for v in vectors {
                    let (slot, _) = nearest(v, &cb, n, mode, luma_weight);
                    clusters[slot as usize].push(*v);
                }
                let mut max_drift: u32 = 0;
                let mut next_cb = cb.clone();
                for (i, c) in clusters.iter().enumerate().take(n) {
                    if !c.is_empty() {
                        let new_centroid = centroid(c, mode);
                        max_drift = max_drift.max(entry_l1_distance(
                            &cb.entries[i],
                            &new_centroid,
                            mode,
                            luma_weight,
                        ));
                        next_cb.entries[i] = new_centroid;
                    }
                    // else: keep cb.entries[i] verbatim (no drift
                    // contribution — empty cluster locks the slot).
                }
                cb = next_cb;
                // Early stop: centroids are essentially stable.
                if max_drift <= eps {
                    break;
                }
            }
            return cb;
        }
        // Empty input but we have a seed → return the seed unchanged.
        if vectors.is_empty() && n > 0 {
            return prev.clone();
        }
    }
    // Round 9 (Lever M): cold-start path. When k-means++ initialisation
    // is enabled, build BOTH the median-cut codebook and the
    // k-means++ + Lloyd codebook, then keep whichever has lower total
    // SSE against the vector population. The k-means++ candidate
    // (Arthur-Vassilvitskii 2007 sampling rule) is provably O(log K)
    // in expectation but is randomised — without the comparison step
    // a single unlucky seed pick can produce a worse codebook than
    // the deterministic median-cut. By always comparing against the
    // median-cut baseline we guarantee round 9 never regresses
    // training SSE vs the round-8 cold-start.
    if kmeans_pp_init && n >= 2 && !vectors.is_empty() {
        let mc_cb = median_cut(vectors, n, mode, luma_weight);
        let pp_cb = kmeans_pp_init_cold(vectors, n, mode, luma_weight, kmeans_pp_lloyd_iter, eps);
        let mc_sse = codebook_total_sse(&mc_cb, vectors, n, mode, luma_weight);
        let pp_sse = codebook_total_sse(&pp_cb, vectors, n, mode, luma_weight);
        if pp_sse < mc_sse {
            return pp_cb;
        }
        return mc_cb;
    }
    median_cut(vectors, n, mode, luma_weight)
}

/// Round 9 helper — total SSE of `vectors` against their nearest
/// codebook centroid. Used by the cold-start hybrid pick (Lever M)
/// to decide whether k-means++ improved on median-cut for this
/// specific vector population.
fn codebook_total_sse(
    cb: &Codebook,
    vectors: &[CodebookEntry],
    n: usize,
    mode: PixelMode,
    luma_weight: u8,
) -> i64 {
    let mut total: i64 = 0;
    for v in vectors {
        let (_slot, err) = nearest(v, cb, n, mode, luma_weight);
        total = total.saturating_add(err);
    }
    total
}

/// Round 9 (Lever M) — k-means++ initialisation followed by Lloyd
/// refinement. Reference: Arthur & Vassilvitskii, "k-means++: The
/// Advantages of Careful Seeding", SODA 2007 (published academic
/// algorithm; no external library source consulted).
///
/// Algorithm (cold start, no seed): sample the first centroid
/// uniformly at random from `vectors`; for each subsequent centroid,
/// compute `D(v) = min_j d²(v, c_j)` (the min squared luma-weighted
/// distance from `v` to any already-chosen centroid), then sample `v`
/// with probability `D(v) / Σ_w D(w)` and add it to the centroid set.
/// After all K centroids are chosen, run up to `lloyd_iter` Lloyd
/// refinement passes (reassign to nearest current centroid, recompute
/// centroids as cluster means, early stop when max per-slot drift ≤
/// `lloyd_eps`).
///
/// Determinism: the sampling RNG is a deterministic xorshift32
/// seeded from a content-derived hash, so identical inputs produce
/// identical codebooks across runs. This matters for reproducible
/// encoding (bit-exact wire output is part of the test contract).
fn kmeans_pp_init_cold(
    vectors: &[CodebookEntry],
    n: usize,
    mode: PixelMode,
    luma_weight: u8,
    lloyd_iter: u8,
    lloyd_eps: u32,
) -> Codebook {
    let mut cb = Codebook::default();
    if vectors.is_empty() || n == 0 {
        return cb;
    }
    let n = n.min(256);
    let mut rng = ContentSeededRng::new(vectors, n, luma_weight);

    // Step 1: pick the first centroid uniformly at random.
    let first_idx = rng.next_index(vectors.len());
    cb.entries[0] = vectors[first_idx];
    let mut chosen: usize = 1;
    if n == 1 {
        return cb;
    }

    // `d2[i]` = min squared luma-weighted distance from `vectors[i]`
    // to any centroid chosen so far. We use the existing
    // `entry_distance` helper (already squared in `Yuv12` mode and L2
    // in `Gray8`). Maintain incrementally: when a new centroid `c`
    // joins the set, update each `d2[i]` to `min(d2[i], d(vectors[i],
    // c))`.
    let mut d2: Vec<i64> = vectors
        .iter()
        .map(|v| entry_distance(v, &cb.entries[0], mode, luma_weight))
        .collect();

    while chosen < n {
        // Step 2: cumulative distribution of D².
        let mut sum: i64 = 0;
        for &d in &d2 {
            sum = sum.saturating_add(d);
        }
        if sum <= 0 {
            // All remaining vectors are already exact-match to some
            // chosen centroid. Fill remaining slots with the first
            // centroid (degenerate but never read by the encoder since
            // these slots attract no vectors).
            for i in chosen..n {
                cb.entries[i] = cb.entries[0];
            }
            break;
        }
        // Sample `r ∈ [0, sum)` from the deterministic RNG.
        let r = rng.next_range_i64(sum);
        let mut acc: i64 = 0;
        let mut pick_idx: usize = vectors.len() - 1;
        for (i, &d) in d2.iter().enumerate() {
            acc = acc.saturating_add(d);
            if acc > r {
                pick_idx = i;
                break;
            }
        }
        cb.entries[chosen] = vectors[pick_idx];
        // Update d² incrementally against the new centroid.
        for (i, v) in vectors.iter().enumerate() {
            let d = entry_distance(v, &cb.entries[chosen], mode, luma_weight);
            if d < d2[i] {
                d2[i] = d;
            }
        }
        chosen += 1;
    }

    // Step 3: Lloyd refinement.
    if lloyd_iter > 0 {
        for _iter in 0..lloyd_iter {
            let mut clusters: Vec<Vec<CodebookEntry>> = vec![Vec::new(); n];
            for v in vectors {
                let (slot, _) = nearest(v, &cb, n, mode, luma_weight);
                clusters[slot as usize].push(*v);
            }
            let mut max_drift: u32 = 0;
            let mut next_cb = cb.clone();
            for (i, c) in clusters.iter().enumerate().take(n) {
                if !c.is_empty() {
                    let new_centroid = centroid(c, mode);
                    max_drift = max_drift.max(entry_l1_distance(
                        &cb.entries[i],
                        &new_centroid,
                        mode,
                        luma_weight,
                    ));
                    next_cb.entries[i] = new_centroid;
                }
            }
            cb = next_cb;
            if max_drift <= lloyd_eps {
                break;
            }
        }
    }
    cb
}

/// Deterministic xorshift32 PRNG seeded from a hash of the input
/// vector population. Used by `kmeans_pp_init_cold` so identical
/// inputs ⇒ identical codebooks ⇒ identical encoded bytes (the
/// project's tests assume reproducible encoding).
struct ContentSeededRng {
    state: u32,
}

impl ContentSeededRng {
    fn new(vectors: &[CodebookEntry], n: usize, luma_weight: u8) -> Self {
        // Mix length, n, luma_weight, and a small subset of the vector
        // population (first, middle, last) into a 32-bit seed. We pick
        // a non-zero default so xorshift32 doesn't lock at 0.
        //
        // Canonicalise `luma_weight = 0` to `1` here to match the
        // "0 → 1 fallback" pattern in `entry_distance` / `extent` —
        // otherwise the RNG seed would differ between the two
        // logically-equivalent inputs, breaking the "luma_weight = 0
        // and 1 produce byte-identical output" contract.
        let canonical_lw = luma_weight.max(1);
        let mut s: u32 = 0x9E37_79B9;
        s = Self::mix(s, vectors.len() as u32);
        s = Self::mix(s, n as u32);
        s = Self::mix(s, u32::from(canonical_lw));
        if !vectors.is_empty() {
            let first = Self::entry_to_u32(&vectors[0]);
            let mid = Self::entry_to_u32(&vectors[vectors.len() / 2]);
            let last = Self::entry_to_u32(&vectors[vectors.len() - 1]);
            s = Self::mix(s, first);
            s = Self::mix(s, mid);
            s = Self::mix(s, last);
        }
        if s == 0 {
            s = 0xDEAD_BEEF;
        }
        ContentSeededRng { state: s }
    }

    fn mix(s: u32, v: u32) -> u32 {
        // 32-bit splitmix-style mix.
        let mut x = s.wrapping_add(v);
        x ^= x >> 16;
        x = x.wrapping_mul(0x85EB_CA6B);
        x ^= x >> 13;
        x = x.wrapping_mul(0xC2B2_AE35);
        x ^= x >> 16;
        x
    }

    fn entry_to_u32(e: &CodebookEntry) -> u32 {
        // Pack first 4 bytes of the entry. Sufficient for hashing.
        u32::from_le_bytes([e.y0, e.y1, e.y2, e.y3])
    }

    /// Standard xorshift32 step.
    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Sample uniformly from `0..len`. `len` must be ≥ 1.
    fn next_index(&mut self, len: usize) -> usize {
        if len <= 1 {
            return 0;
        }
        // Modulo bias is acceptable for our use: cluster initialisation
        // is statistical, and the deterministic seed already removes
        // any guarantee of perfect uniformity over independent runs.
        (self.next_u32() as usize) % len
    }

    /// Sample uniformly from `0..bound`. `bound` must be > 0.
    fn next_range_i64(&mut self, bound: i64) -> i64 {
        if bound <= 1 {
            return 0;
        }
        // Use 64 bits of entropy for the modulo to limit bias on large
        // bounds (codebook-distance sums can run into the i64 range on
        // big strips).
        let hi = u64::from(self.next_u32()) << 32;
        let lo = u64::from(self.next_u32());
        let r = (hi | lo) as i64;
        let r = r.rem_euclid(bound);
        r.max(0)
    }
}

/// L1 (Manhattan) distance between two codebook entries summed across
/// all dims; used for Lloyd early-stop convergence detection.
///
/// Round 5 (Lever F): Y-dim contributions are weighted by
/// `luma_weight`. A `luma_weight` of `1` reproduces the round-4
/// isotropic metric; the default `2` weighs Y dims twice as much as
/// U/V dims. Treats `0` as `1`. In `Gray8` mode `luma_weight` is a
/// no-op (no chroma dims to compare against).
fn entry_l1_distance(
    a: &CodebookEntry,
    b: &CodebookEntry,
    mode: PixelMode,
    luma_weight: u8,
) -> u32 {
    let w = luma_weight.max(1) as u32;
    let mut d = w
        * ((i32::from(a.y0) - i32::from(b.y0)).unsigned_abs()
            + (i32::from(a.y1) - i32::from(b.y1)).unsigned_abs()
            + (i32::from(a.y2) - i32::from(b.y2)).unsigned_abs()
            + (i32::from(a.y3) - i32::from(b.y3)).unsigned_abs());
    if let PixelMode::Yuv12 = mode {
        d += (i32::from(a.u) - i32::from(b.u)).unsigned_abs()
            + (i32::from(a.v) - i32::from(b.v)).unsigned_abs();
    }
    d
}

/// Round 4 Lever E — **Linde-Buzo-Gray (LBG) split refinement** applied
/// to an already-built codebook. Each pass:
///
/// 1. Assigns every vector to its nearest current codebook slot and
///    measures the per-slot SSE (sum of squared distances of cluster
///    members to centroid).
/// 2. Identifies the highest-SSE slot whose population is ≥ 2 ("the
///    splitter") and the lowest-population slot ("the donor"). When the
///    splitter's SSE strictly exceeds the donor's, the donor's slot is
///    replaced by a perturbed copy of the splitter's centroid (perturbed
///    by ±1 along the cluster's widest dimension), and the splitter's
///    own centroid is left in place. A single full Lloyd assignment +
///    recentroid pass then re-balances both slots and lets neighbours
///    absorb the freed vectors.
/// 3. Total SSE is recomputed; if it strictly decreased, the pass kept
///    its improvement and we proceed to the next pass. If it didn't, we
///    revert and stop (no further LBG passes will help).
///
/// Reference: Linde, Buzo, Gray (1980) "An Algorithm for Vector
/// Quantizer Design", IEEE Trans. Communications 28(1) — published
/// VQ-design math, no proprietary source consulted.
///
/// The function is a **no-op** when `n ≤ 1`, when `vectors.is_empty()`,
/// or when `max_passes == 0`.
fn lbg_refine_codebook(
    cb: &mut Codebook,
    vectors: &[CodebookEntry],
    n: usize,
    mode: PixelMode,
    max_passes: u8,
    luma_weight: u8,
) {
    if max_passes == 0 || n <= 1 || vectors.is_empty() {
        return;
    }
    let dims = match mode {
        PixelMode::Yuv12 => 6,
        PixelMode::Gray8 => 4,
    };
    for _pass in 0..max_passes {
        // Assign every vector to its nearest current slot; collect both
        // total SSE and per-slot population + per-slot SSE.
        let mut clusters: Vec<Vec<CodebookEntry>> = vec![Vec::new(); n];
        let mut total_sse_before: i64 = 0;
        for v in vectors {
            let (slot, err) = nearest(v, cb, n, mode, luma_weight);
            clusters[slot as usize].push(*v);
            total_sse_before = total_sse_before.saturating_add(err);
        }
        // Compute per-slot SSE for splitter selection. Per-slot SSE is
        // recomputed against the slot's CURRENT centroid (not a refreshed
        // mean of its members) so we measure the slot's actual coding
        // distortion as the encoder will see it on this iteration.
        let mut per_slot_sse: Vec<i64> = vec![0; n];
        for (i, c) in clusters.iter().enumerate().take(n) {
            let centre = cb.entries[i];
            for v in c {
                per_slot_sse[i] =
                    per_slot_sse[i].saturating_add(entry_distance(v, &centre, mode, luma_weight));
            }
        }
        // Pick the highest-SSE slot with ≥ 2 members ("splitter") and
        // the lowest-population slot ("donor"). Donor must be DIFFERENT
        // from splitter, and donor's SSE must be strictly less than
        // splitter's SSE (otherwise the swap would never lower total
        // distortion).
        let mut splitter: Option<usize> = None;
        let mut splitter_sse: i64 = -1;
        for i in 0..n {
            if clusters[i].len() >= 2 && per_slot_sse[i] > splitter_sse {
                splitter_sse = per_slot_sse[i];
                splitter = Some(i);
            }
        }
        let Some(splitter_idx) = splitter else {
            break;
        };
        let mut donor: Option<usize> = None;
        let mut donor_pop: usize = usize::MAX;
        let mut donor_sse: i64 = i64::MAX;
        for i in 0..n {
            if i == splitter_idx {
                continue;
            }
            let pop = clusters[i].len();
            // Prefer the smallest-population slot; tiebreak on smallest
            // per-slot SSE (least useful slot to keep).
            if pop < donor_pop || (pop == donor_pop && per_slot_sse[i] < donor_sse) {
                donor_pop = pop;
                donor_sse = per_slot_sse[i];
                donor = Some(i);
            }
        }
        let Some(donor_idx) = donor else {
            break;
        };
        // Heuristic guard: a donor whose own SSE is already comparable
        // to the splitter's is a poor donor (split won't free up enough
        // assignment mass to lower total SSE). Skip the pass.
        if donor_sse >= splitter_sse {
            break;
        }
        // Find the splitter cluster's widest dimension; perturbation
        // direction is ±1 along that dim. Magnitude 1 keeps the
        // perturbation small so subsequent Lloyd re-centroids quickly
        // resolve the two new clusters.
        let mut wide_dim = 0usize;
        let mut wide_ext: i32 = -1;
        for d in 0..dims {
            let (lo, hi) = extent(&clusters[splitter_idx], d);
            let ext = hi - lo;
            if ext > wide_ext {
                wide_ext = ext;
                wide_dim = d;
            }
        }
        if wide_ext <= 0 {
            // Splitter is degenerate (all members identical) — splitting
            // it can't help. Try again with no further passes.
            break;
        }
        // Snapshot the current codebook so we can revert if total SSE
        // doesn't strictly improve. Only `splitter` and `donor` slots
        // change in this pass; snapshot only those two.
        let prev_splitter = cb.entries[splitter_idx];
        let prev_donor = cb.entries[donor_idx];
        // Perturb the splitter centroid into the donor slot. We perturb
        // BOTH slots: the donor gets `+1` along wide_dim, the splitter
        // gets `-1`. Symmetric perturbation around the original centroid
        // gives the subsequent Lloyd pass two seeds straddling the
        // cluster's principal direction.
        let mut new_splitter = prev_splitter;
        let mut new_donor = prev_splitter;
        perturb_dim(&mut new_splitter, wide_dim, -1);
        perturb_dim(&mut new_donor, wide_dim, 1);
        cb.entries[splitter_idx] = new_splitter;
        cb.entries[donor_idx] = new_donor;
        // One Lloyd pass to rebalance assignments + recentroid every
        // slot using the new pair.
        let mut new_clusters: Vec<Vec<CodebookEntry>> = vec![Vec::new(); n];
        let mut total_sse_after: i64 = 0;
        for v in vectors {
            let (slot, err) = nearest(v, cb, n, mode, luma_weight);
            new_clusters[slot as usize].push(*v);
            total_sse_after = total_sse_after.saturating_add(err);
        }
        let mut next_cb = cb.clone();
        for (i, c) in new_clusters.iter().enumerate().take(n) {
            if !c.is_empty() {
                next_cb.entries[i] = centroid(c, mode);
            }
            // else: keep prior value (unreferenced slot, no harm).
        }
        // Recompute total SSE against the recentroided codebook — this
        // is the SSE the encoder will actually see when it picks
        // nearest-neighbours for MB classification.
        let mut total_sse_final: i64 = 0;
        for v in vectors {
            let (_slot, err) = nearest(v, &next_cb, n, mode, luma_weight);
            total_sse_final = total_sse_final.saturating_add(err);
        }
        if total_sse_final < total_sse_before {
            *cb = next_cb;
            // Continue to next pass.
        } else {
            // Revert and stop: no further passes can help.
            cb.entries[splitter_idx] = prev_splitter;
            cb.entries[donor_idx] = prev_donor;
            break;
        }
    }
}

/// Helper for [`lbg_refine_codebook`]: nudge a codebook entry by `±delta`
/// along the given dimension index (0=Y0, 1=Y1, 2=Y2, 3=Y3, 4=U, 5=V).
/// Wraps via saturation (u8 / i8 clamp) so a perturbation that would
/// push past the type boundary clamps to the boundary instead of
/// overflowing.
fn perturb_dim(e: &mut CodebookEntry, dim: usize, delta: i32) {
    match dim {
        0 => e.y0 = (i32::from(e.y0) + delta).clamp(0, 255) as u8,
        1 => e.y1 = (i32::from(e.y1) + delta).clamp(0, 255) as u8,
        2 => e.y2 = (i32::from(e.y2) + delta).clamp(0, 255) as u8,
        3 => e.y3 = (i32::from(e.y3) + delta).clamp(0, 255) as u8,
        4 => e.u = (i32::from(e.u) + delta).clamp(-128, 127) as i8,
        5 => e.v = (i32::from(e.v) + delta).clamp(-128, 127) as i8,
        _ => {}
    }
}

/// Build a codebook of up to `n` entries from the population
/// `vectors`. Median-cut: recursively bisect the population along the
/// dimension of greatest range, each cut producing two sub-clusters.
/// Each leaf cluster contributes one codebook entry — the centroid of
/// its members.
///
/// Round 5 (Lever G — **luma-prioritized split**): when comparing the
/// per-dim extents to pick a split dimension, each Y-dim extent
/// (indices 0..=3) is multiplied by `luma_weight` before being compared
/// against the U/V-dim extents (indices 4..=5). This biases the
/// initial bisection toward Y-axis cuts when Y and U/V extents are
/// otherwise comparable — under PSNR_Y, packing the codebook tightly
/// in Y is more valuable than packing it tightly in U/V. `luma_weight
/// = 1` reproduces the round-4 isotropic split. `luma_weight = 0` is
/// treated as `1` (no-op).
fn median_cut(vectors: &[CodebookEntry], n: usize, mode: PixelMode, luma_weight: u8) -> Codebook {
    let mut cb = Codebook::default();
    if vectors.is_empty() || n == 0 {
        return cb;
    }
    // Build an initial single cluster.
    let dims = match mode {
        PixelMode::Yuv12 => 6,
        PixelMode::Gray8 => 4,
    };
    let w = luma_weight.max(1) as i32;
    let mut clusters: Vec<Vec<CodebookEntry>> = vec![vectors.to_vec()];
    while clusters.len() < n {
        // Find cluster with largest weighted extent along any dimension.
        // Y-dims (0..=3) get a `luma_weight` multiplier; U/V-dims
        // (4..=5) stay at weight 1. We compare on the weighted score
        // but still split on the un-weighted dim values themselves —
        // the weight only affects which dim wins.
        let mut best_idx = None;
        let mut best_score: i32 = 0;
        let mut best_dim = 0;
        for (ci, c) in clusters.iter().enumerate() {
            if c.len() < 2 {
                continue;
            }
            for d in 0..dims {
                let (lo, hi) = extent(c, d);
                let ext = hi - lo;
                let weighted = if d < 4 { ext.saturating_mul(w) } else { ext };
                if weighted > best_score {
                    best_score = weighted;
                    best_idx = Some(ci);
                    best_dim = d;
                }
            }
        }
        let Some(idx) = best_idx else {
            break;
        };
        if best_score == 0 {
            break;
        }
        let mut cluster = std::mem::take(&mut clusters[idx]);
        cluster.sort_by_key(|e| dim_value(e, best_dim));
        let mid = cluster.len() / 2;
        let right = cluster.split_off(mid);
        clusters[idx] = cluster;
        clusters.push(right);
    }
    // Compute centroids and write to codebook entries.
    for (i, c) in clusters.iter().enumerate().take(n) {
        if c.is_empty() {
            continue;
        }
        cb.entries[i] = centroid(c, mode);
    }
    cb
}

fn dim_value(e: &CodebookEntry, d: usize) -> i32 {
    match d {
        0 => i32::from(e.y0),
        1 => i32::from(e.y1),
        2 => i32::from(e.y2),
        3 => i32::from(e.y3),
        4 => i32::from(e.u),
        5 => i32::from(e.v),
        _ => 0,
    }
}

fn extent(c: &[CodebookEntry], d: usize) -> (i32, i32) {
    let mut lo = i32::MAX;
    let mut hi = i32::MIN;
    for e in c {
        let v = dim_value(e, d);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (lo, hi)
}

fn centroid(c: &[CodebookEntry], mode: PixelMode) -> CodebookEntry {
    let n = c.len() as i32;
    let mut s = [0i64; 6];
    for e in c {
        s[0] += i64::from(e.y0);
        s[1] += i64::from(e.y1);
        s[2] += i64::from(e.y2);
        s[3] += i64::from(e.y3);
        s[4] += i64::from(e.u);
        s[5] += i64::from(e.v);
    }
    let div = i64::from(n);
    let y0 = (s[0] / div) as u8;
    let y1 = (s[1] / div) as u8;
    let y2 = (s[2] / div) as u8;
    let y3 = (s[3] / div) as u8;
    let (u, v) = match mode {
        PixelMode::Yuv12 => ((s[4] / div) as i8, (s[5] / div) as i8),
        PixelMode::Gray8 => (0, 0),
    };
    CodebookEntry {
        y0,
        y1,
        y2,
        y3,
        u,
        v,
    }
}

// ---------------------------------------------------------------------------
// Per-MB nearest-neighbour selection
// ---------------------------------------------------------------------------

/// Squared distance between two codebook entries. Round 5 (Lever F):
/// each Y-dim squared-error contribution is multiplied by
/// `luma_weight`. `luma_weight = 1` reproduces the round-4 isotropic
/// metric; `0` is treated as `1` (no luma weight). In `Gray8` mode
/// `luma_weight` is a no-op (no chroma dims to compare against).
fn entry_distance(a: &CodebookEntry, b: &CodebookEntry, mode: PixelMode, luma_weight: u8) -> i64 {
    let w = luma_weight.max(1) as i64;
    let dy0 = i64::from(a.y0) - i64::from(b.y0);
    let dy1 = i64::from(a.y1) - i64::from(b.y1);
    let dy2 = i64::from(a.y2) - i64::from(b.y2);
    let dy3 = i64::from(a.y3) - i64::from(b.y3);
    let mut d = w * (dy0 * dy0 + dy1 * dy1 + dy2 * dy2 + dy3 * dy3);
    if let PixelMode::Yuv12 = mode {
        let du = i64::from(a.u) - i64::from(b.u);
        let dv = i64::from(a.v) - i64::from(b.v);
        d += du * du + dv * dv;
    }
    d
}

fn nearest(
    target: &CodebookEntry,
    cb: &Codebook,
    n: usize,
    mode: PixelMode,
    luma_weight: u8,
) -> (u8, i64) {
    let mut best_idx = 0u8;
    let mut best_err = i64::MAX;
    for i in 0..n.min(256) {
        let d = entry_distance(target, &cb.entries[i], mode, luma_weight);
        if d < best_err {
            best_err = d;
            best_idx = i as u8;
        }
    }
    (best_idx, best_err)
}

fn pick_v4(
    target: &[CodebookEntry; 4],
    cb: &Codebook,
    n: usize,
    luma_weight: u8,
) -> ([u8; 4], i64) {
    let mut idx = [0u8; 4];
    let mut err = 0i64;
    for sub in 0..4 {
        let (i, e) = nearest(&target[sub], cb, n, PixelMode::Yuv12, luma_weight);
        idx[sub] = i;
        err += e;
    }
    (idx, err)
}

fn pick_v1(
    target: &CodebookEntry,
    cb: &Codebook,
    n: usize,
    mode: PixelMode,
    luma_weight: u8,
) -> (u8, i64) {
    nearest(target, cb, n, mode, luma_weight)
}

/// Round-3 (round-47) Lagrangian V1/V4 RDO helper: compute pixel-domain
/// Y SSE for the V4 and V1 reconstructions of one macroblock against the
/// raw pixel Y values (which are stored in `mb_v4` — each sub-block of
/// `mb_v4` carries the four raw Y samples for its 2×2 pixel patch).
///
/// Returns `(d_v4_y, d_v1_y)` — both in pixel-Y SSE units (sum of
/// squared differences over 16 Y values in the 4×4 MB).
///
/// Why pixel-Y SSE and not the codebook-distance metric used by
/// `pick_v4`/`pick_v1`: the codebook-distance metric measures how
/// closely the codebook approximates the *MB's representative vector*
/// (four sub-block tuples for V4; one MB-averaged tuple for V1) — V1's
/// metric inherently understates pixel error because the MB-averaged Y
/// vector hides the within-quadrant variance the V1 reconstruction
/// can never recover. Pixel-Y SSE captures both the codebook
/// quantisation error AND the V1 within-quadrant smoothing error,
/// which is the actual visible distortion the decoder produces.
///
/// Chroma residuals are deliberately excluded: V4 and V1 in 12-bit YUV
/// mode both carry chroma at sub-block granularity (V4) or MB
/// granularity (V1), and the V1 chroma loss is dominated by the Y
/// detail loss anyway — including it would only add a small constant
/// favouring V4 that does not change the RD trade-off meaningfully.
fn rdo_pixel_y_sse(
    mb_v4: &[CodebookEntry; 4],
    v4_idx: &[u8; 4],
    v4_cb: &Codebook,
    v1_idx: u8,
    v1_cb: &Codebook,
) -> (i64, i64) {
    let mut d_v4: i64 = 0;
    let mut d_v1: i64 = 0;
    let v1_entry = &v1_cb.entries[v1_idx as usize];
    // Each "sub-block" in the V4 view (sub_idx 0..4) covers the same
    // 2×2 pixel patch as V1's "quadrant" of the same index (per the
    // sample_v4_block / sample_v1_block layout). So the source pixel
    // Y values are `mb_v4[sub_idx].Y[pixel_idx]` for pixel_idx 0..4.
    //
    // V1 reconstructs every pixel in quadrant `q` to v1_entry.Y[q]
    // (with Y[q] meaning the codebook entry's Y0/Y1/Y2/Y3 component
    // corresponding to the quadrant index, NOT the per-pixel index).
    // V4 reconstructs each pixel in sub-block `s` at position `p` to
    // v4_entry_for_subblock.Y[p].
    for sub_idx in 0..4 {
        let src = &mb_v4[sub_idx];
        let src_ys = [src.y0, src.y1, src.y2, src.y3];
        let v4_e = &v4_cb.entries[v4_idx[sub_idx] as usize];
        let v4_ys = [v4_e.y0, v4_e.y1, v4_e.y2, v4_e.y3];
        // V1 quadrant Y value depends on which quadrant `sub_idx` is.
        let v1_quad_y = match sub_idx {
            0 => v1_entry.y0,
            1 => v1_entry.y1,
            2 => v1_entry.y2,
            _ => v1_entry.y3,
        };
        for pixel_idx in 0..4 {
            let s = i64::from(src_ys[pixel_idx]);
            let r4 = i64::from(v4_ys[pixel_idx]);
            let r1 = i64::from(v1_quad_y);
            let d4 = s - r4;
            let d1 = s - r1;
            d_v4 += d4 * d4;
            d_v1 += d1 * d1;
        }
    }
    (d_v4, d_v1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CinepakDecoder;

    /// Round-trip: encode a synthesised RGB frame, decode it, and
    /// confirm decoded pixels are within a small per-channel tolerance.
    /// The tolerance accounts for codebook quantisation error; with 64
    /// entries and a 16×16 frame of 4 distinct colour blocks, the
    /// quantiser should reproduce each block exactly.
    #[test]
    fn rgb24_roundtrip_4_color_blocks() {
        // 16×16 frame: four 8×8 quadrants, each one solid colour.
        let w = 16usize;
        let h = 16usize;
        let mut rgb = vec![0u8; w * h * 3];
        let colors = [
            (255, 0, 0),   // red TL
            (0, 255, 0),   // green TR
            (0, 0, 255),   // blue BL
            (200, 200, 0), // yellow BR
        ];
        for r in 0..h {
            for c in 0..w {
                let q = (r / 8) * 2 + (c / 8); // 0..3
                let off = r * w * 3 + c * 3;
                rgb[off] = colors[q].0;
                rgb[off + 1] = colors[q].1;
                rgb[off + 2] = colors[q].2;
            }
        }
        let bytes = encode_rgb24(&rgb, w as u32, h as u32, EncoderOptions::default()).unwrap();
        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.width, w as u32);
        assert_eq!(f.height, h as u32);
        let s = f.stride();
        let p = f.pixels();
        // Sample one pixel in each quadrant; codebook should reproduce
        // the dominant block colour within YUV-quantisation tolerance.
        for q in 0..4 {
            let qr = (q / 2) * 8 + 2;
            let qc = (q % 2) * 8 + 2;
            let off = qr * s + qc * 3;
            let r = p[off] as i32;
            let g = p[off + 1] as i32;
            let b = p[off + 2] as i32;
            let (er, eg, eb) = (colors[q].0 as i32, colors[q].1 as i32, colors[q].2 as i32);
            // The YUV inverse of (255, 0, 0) etc. has up to ~1 unit
            // of round-off in Y; allow ±8 per channel.
            assert!((r - er).abs() <= 8, "q{q} R={r} expect {er}");
            assert!((g - eg).abs() <= 8, "q{q} G={g} expect {eg}");
            assert!((b - eb).abs() <= 8, "q{q} B={b} expect {eb}");
        }
    }

    /// Round-trip: 8-bit grayscale.
    #[test]
    fn gray8_roundtrip_4_blocks() {
        let w = 16usize;
        let h = 16usize;
        let mut gray = vec![0u8; w * h];
        let lums = [40u8, 80, 160, 220];
        for r in 0..h {
            for c in 0..w {
                let q = (r / 8) * 2 + (c / 8);
                gray[r * w + c] = lums[q];
            }
        }
        let bytes = encode_gray8(&gray, w as u32, h as u32, EncoderOptions::default()).unwrap();
        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.pixel_format, crate::CinepakPixelFormat::Gray8);
        assert_eq!(f.stride(), w);
        let p = f.pixels();
        for q in 0..4 {
            let qr = (q / 2) * 8 + 2;
            let qc = (q % 2) * 8 + 2;
            let v = p[qr * w + qc];
            assert!(
                (v as i32 - lums[q] as i32).abs() <= 4,
                "q{q} got {v} expect {}",
                lums[q]
            );
        }
    }

    /// Encoder rejects non-multiple-of-4 dims (matches header parser).
    #[test]
    fn rejects_misaligned_dims() {
        let rgb = vec![0u8; 5 * 4 * 3];
        let r = encode_rgb24(&rgb, 5, 4, EncoderOptions::default());
        assert!(r.is_err());
    }

    /// Encoder produces a stream whose declared frame_length matches
    /// its byte length exactly.
    #[test]
    fn frame_length_matches_buffer_size() {
        let rgb = vec![128u8; 8 * 8 * 3];
        let bytes = encode_rgb24(&rgb, 8, 8, EncoderOptions::default()).unwrap();
        let h = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(h.frame_length as usize, bytes.len());
        assert_eq!(h.width, 8);
        assert_eq!(h.height, 8);
        assert_eq!(h.strip_count, 1);
    }

    /// Multi-strip planning: 8 MB rows split into 3 strips ⇒ 3+3+2.
    #[test]
    fn plan_strips_distributes_remainder_to_first() {
        let plans = plan_strips(8, 3);
        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0].y_top, 0);
        assert_eq!(plans[0].y_bottom, 12); // 3 MB rows × 4 px
        assert_eq!(plans[1].y_top, 12);
        assert_eq!(plans[1].y_bottom, 24); // 3 MB rows
        assert_eq!(plans[2].y_top, 24);
        assert_eq!(plans[2].y_bottom, 32); // 2 MB rows
    }

    /// Multi-strip planning: requested strips clamped to MB-row count.
    #[test]
    fn plan_strips_clamps_to_mb_rows() {
        let plans = plan_strips(2, 8);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].y_bottom, 4);
        assert_eq!(plans[1].y_bottom, 8);
    }

    /// Multi-strip encode: a 32×32 frame with `strip_count = 4` should
    /// produce 4 strips that round-trip through the decoder.
    #[test]
    fn encode_multi_strip_4_strips_32x32() {
        let w = 32usize;
        let h = 32usize;
        let mut rgb = vec![0u8; w * h * 3];
        // Vertical gradient: each strip should learn its own band.
        for r in 0..h {
            let g = (r * 8) as u8;
            for c in 0..w {
                let off = r * w * 3 + c * 3;
                rgb[off] = g;
                rgb[off + 1] = g;
                rgb[off + 2] = g;
            }
        }
        let opts = EncoderOptions {
            v4_entries: 8,
            v1_entries: 8,
            strip_count: 4,
            skip_threshold: 64.0,
            ..EncoderOptions::default()
        };
        let bytes = encode_rgb24(&rgb, w as u32, h as u32, opts).unwrap();
        let h_parsed = FrameHeader::parse(&bytes).unwrap();
        assert_eq!(h_parsed.strip_count, 4);
        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.width, 32);
        assert_eq!(f.height, 32);
        // Spot check: row 4 should be ~32, row 28 should be ~224.
        let p = f.pixels();
        let s = f.stride();
        let r4 = p[4 * s + 8] as i32;
        let r28 = p[28 * s + 8] as i32;
        assert!(r4 < 80, "row 4 luma {r4} should be dark");
        assert!(r28 > 160, "row 28 luma {r28} should be bright");
    }

    /// Inter encode: an unchanged frame should produce an inter frame
    /// where most macroblocks are SKIP.
    #[test]
    fn encode_inter_unchanged_frame_is_mostly_skip() {
        let w = 16usize;
        let h = 16usize;
        // Solid mid-gray — uniform.
        let rgb = vec![128u8; w * h * 3];
        let bytes_intra =
            encode_rgb24(&rgb, w as u32, h as u32, EncoderOptions::default()).unwrap();
        let mut dec = CinepakDecoder::new();
        let prev = dec.decode_frame(&bytes_intra, None).unwrap();

        // Inter encode the same frame again, against `prev`.
        let opts = EncoderOptions {
            v4_entries: 8,
            v1_entries: 8,
            strip_count: 1,
            skip_threshold: 32.0,
            ..EncoderOptions::default()
        };
        let bytes_inter = encode_rgb24_inter(&rgb, &prev, w as u32, h as u32, opts).unwrap();
        // Verify it carries an inter strip (0x1100).
        let strip_off = FRAME_HEADER_SIZE;
        assert_eq!(bytes_inter[strip_off], 0x11);
        assert_eq!(bytes_inter[strip_off + 1], 0x00);

        // Decoding should still produce the same pixels.
        let mut dec2 = CinepakDecoder::new();
        // Seed dec2 with the intra frame so its prev-frame state is set.
        let _ = dec2.decode_frame(&bytes_intra, None).unwrap();
        let f2 = dec2.decode_frame(&bytes_inter, None).unwrap();
        let p = f2.pixels();
        let s = f2.stride();
        // All pixels should still be mid-gray (within tolerance).
        for r in 0..h {
            for c in 0..w {
                let off = r * s + c * 3;
                assert!(
                    (p[off] as i32 - 128).abs() <= 12,
                    "({r},{c}) R={} expected ~128",
                    p[off]
                );
            }
        }
    }

    /// Quality knob: larger `quality` should produce strictly more
    /// codebook entries (within the 8..=256 range).
    #[test]
    fn from_quality_monotonic_codebook() {
        let q0 = EncoderOptions::from_quality(0);
        let q50 = EncoderOptions::from_quality(50);
        let q100 = EncoderOptions::from_quality(100);
        assert!(q0.v4_entries < q50.v4_entries);
        assert!(q50.v4_entries < q100.v4_entries);
        assert_eq!(q0.v4_entries, q0.v1_entries);
        assert_eq!(q100.v4_entries, 256);
        assert!(q100.skip_threshold < q0.skip_threshold);
        assert!(q100.strip_count >= q0.strip_count);
    }

    /// Quality knob: encode at q=0 (smallest codebook) and confirm
    /// roundtrip still works (lossy but correct shape).
    #[test]
    fn from_quality_encodes_at_min_q() {
        let w = 16usize;
        let h = 16usize;
        let rgb = vec![100u8; w * h * 3];
        let opts = EncoderOptions::from_quality(0);
        assert_eq!(opts.v4_entries, 8);
        let bytes = encode_rgb24(&rgb, w as u32, h as u32, opts).unwrap();
        let mut dec = CinepakDecoder::new();
        let f = dec.decode_frame(&bytes, None).unwrap();
        assert_eq!(f.width, 16);
    }

    /// Validation: skip_threshold must be finite & non-negative.
    #[test]
    fn rejects_nan_skip_threshold() {
        let opts = EncoderOptions {
            v4_entries: 8,
            v1_entries: 8,
            strip_count: 1,
            skip_threshold: f32::NAN,
            ..EncoderOptions::default()
        };
        let rgb = vec![0u8; 8 * 8 * 3];
        assert!(encode_rgb24(&rgb, 8, 8, opts).is_err());
    }

    /// Validation: strip_count = 0 is rejected.
    #[test]
    fn rejects_zero_strip_count() {
        let opts = EncoderOptions {
            v4_entries: 8,
            v1_entries: 8,
            strip_count: 0,
            skip_threshold: 64.0,
            ..EncoderOptions::default()
        };
        let rgb = vec![0u8; 8 * 8 * 3];
        assert!(encode_rgb24(&rgb, 8, 8, opts).is_err());
    }

    /// Inter-encode error path: prev-frame size mismatch.
    #[test]
    fn rejects_prev_size_mismatch() {
        let rgb_a = vec![0u8; 8 * 8 * 3];
        let bytes = encode_rgb24(&rgb_a, 8, 8, EncoderOptions::default()).unwrap();
        let mut dec = CinepakDecoder::new();
        let prev = dec.decode_frame(&bytes, None).unwrap();
        let rgb_b = vec![0u8; 16 * 16 * 3];
        let r = encode_rgb24_inter(&rgb_b, &prev, 16, 16, EncoderOptions::default());
        assert!(r.is_err());
    }
}
