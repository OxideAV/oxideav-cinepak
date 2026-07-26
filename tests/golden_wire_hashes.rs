//! Round 430 — golden wire-byte pins for the encoder.
//!
//! This round is a **performance** round: every optimization applied to
//! the encoder hot paths (codebook training, nearest-neighbour search,
//! RDO, strip assembly) must be *output-invariant*. These tests pin the
//! exact wire bytes (FNV-1a-64 over the concatenated frame payloads) of
//! a representative encode matrix so any behavioural drift — however
//! subtle — turns into a hard test failure instead of a silent quality
//! or compatibility change.
//!
//! Matrix design notes:
//!
//! - Every scenario builds its `EncoderOptions` **explicitly** (no
//!   `from_quality`, which routes through `f32::powf`; transcendental
//!   libm results are the one place cross-platform bit-drift is
//!   conceivable). Everything the encoder computes from these option
//!   vectors is integer or basic-IEEE-arithmetic (`+ - * /`, `sqrt`),
//!   which is bit-deterministic across conforming platforms.
//! - The matrix covers: intra + inter (stateful GOP with cross-frame
//!   codebook persistence + selective update), 1/2/3-strip grids,
//!   `vintage_compat` header-only chunk emission, max-size codebooks,
//!   the training-minimal legacy path (median-cut only, RDO off), all
//!   three picker tiers (round-6/7/8), grayscale intra + GOP, stale-slot
//!   reclamation, and the per-frame byte-budget rate-control path.
//! - The FILM / Saturn / 3DO deviants are *decode-side* configurations
//!   (`DeviantConfig`); the encoder emits standard CVID only, so no
//!   deviant rows exist here.
//! - Every pinned stream must also stay **fully clean** under the
//!   42-rule conformance linter (zero findings, warnings included) —
//!   asserted alongside each hash.
//!
//! Regenerating (only legitimate after an *intentional* wire change,
//! never in a performance round):
//!
//! ```sh
//! OXIDEAV_CINEPAK_GOLDEN_PRINT=1 cargo test --test golden_wire_hashes -- --nocapture
//! ```

use oxideav_cinepak::{
    encode_gray8, encode_rgb24, encode_rgb24_round6, encode_rgb24_round7, encode_rgb24_round8,
    lint_sequence, CinepakEncoder, EncoderOptions, LintOptions,
};

// ---------------------------------------------------------------------------
// Hashing + fixtures
// ---------------------------------------------------------------------------

/// FNV-1a 64-bit over a byte stream. Small, dependency-free, and more
/// than collision-resistant enough for a regression pin (any drift in
/// the ~1..40 KiB wire outputs flips the digest).
struct Fnv1a64(u64);

impl Fnv1a64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Fnv1a64(Self::OFFSET)
    }

    fn update(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(Self::PRIME);
        }
        self.0 = h;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Digest a frame sequence: each frame contributes its length (LE u64)
/// then its bytes, so frame-boundary shifts can't alias.
fn digest_frames(frames: &[Vec<u8>]) -> u64 {
    let mut h = Fnv1a64::new();
    for f in frames {
        h.update(&(f.len() as u64).to_le_bytes());
        h.update(f);
    }
    h.finish()
}

fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

/// Deterministic RGB gradient + low-amplitude noise (same recipe as the
/// bench/profile fixtures), with a per-frame phase shift so successive
/// frames of a GOP produce a slow horizontal pan — real SKIP / inter /
/// selective-update traffic without scene cuts.
fn rgb_frame(width: usize, height: usize, phase: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 3];
    let mut state: u32 = 0x1234_5678 ^ (phase as u32).wrapping_mul(0x9E37_79B9);
    for r in 0..height {
        for c in 0..width {
            let cc = c + phase; // pan
            let base_y = ((r * 255) / height.max(1)) as u32;
            let base_x = ((cc * 255) / width.max(1)).min(255) as u32;
            let r_v = ((base_x + base_y) / 2).min(255) as u8;
            let g_v = base_y.min(255) as u8;
            let b_v = base_x.min(255) as u8;
            let noise = xorshift_byte(&mut state) & 0x07;
            let idx = (r * width + c) * 3;
            out[idx] = r_v.wrapping_add(noise);
            out[idx + 1] = g_v.wrapping_add(noise);
            out[idx + 2] = b_v.wrapping_add(noise);
        }
    }
    out
}

fn gray_frame(width: usize, height: usize, phase: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    let mut state: u32 = 0x9abc_def0 ^ (phase as u32).wrapping_mul(0x85EB_CA6B);
    for r in 0..height {
        for c in 0..width {
            let cc = c + phase;
            let base = ((r * 255) / height.max(1) + ((cc * 255) / width.max(1)).min(255)) / 2;
            let noise = xorshift_byte(&mut state) & 0x07;
            out[r * width + c] = (base as u8).wrapping_add(noise);
        }
    }
    out
}

/// Round-430 default options, written out field-by-field (identical to
/// `EncoderOptions::default()` today — spelled explicitly so the pin
/// does not silently move if a future round changes the defaults; a
/// default change should *consciously* re-pin).
fn opts_default() -> EncoderOptions {
    EncoderOptions {
        v4_entries: 64,
        v1_entries: 64,
        strip_count: 1,
        skip_threshold: 64.0,
        lloyd_max_iter: 2,
        lloyd_eps: 1,
        stale_slot_threshold: Some(8),
        rdo_lambda: Some(5.0),
        lbg_max_passes: 8,
        luma_weight: 2,
        pcl_max_iter: 2,
        kmeans_pp_init: true,
        kmeans_pp_lloyd_iter: 4,
        vintage_compat: false,
    }
}

/// Training-minimal legacy path: median-cut only, isotropic metric,
/// legacy distance-sum V1/V4 selection. Exercises the code paths the
/// default vector skips.
fn opts_legacy_minimal() -> EncoderOptions {
    EncoderOptions {
        rdo_lambda: None,
        lbg_max_passes: 0,
        luma_weight: 1,
        pcl_max_iter: 0,
        kmeans_pp_init: false,
        kmeans_pp_lloyd_iter: 0,
        lloyd_max_iter: 0,
        ..opts_default()
    }
}

// ---------------------------------------------------------------------------
// Scenario runners
// ---------------------------------------------------------------------------

fn assert_lint_clean(name: &str, frames: &[Vec<u8>]) {
    let reports = lint_sequence(frames.iter().map(|f| f.as_slice()), &LintOptions::default());
    for (i, rep) in reports.iter().enumerate() {
        assert!(
            rep.is_clean(),
            "golden scenario `{name}` frame {i} has lint findings: {:?}",
            rep.issues()
        );
    }
}

/// One scenario = a name + the produced frame sequence. Digest +
/// lint-cleanliness are asserted together against the pin table.
fn check(name: &str, frames: Vec<Vec<u8>>, expected: u64) {
    assert!(
        !frames.is_empty() && frames.iter().all(|f| !f.is_empty()),
        "golden scenario `{name}` produced an empty frame"
    );
    assert_lint_clean(name, &frames);
    let got = digest_frames(&frames);
    if std::env::var_os("OXIDEAV_CINEPAK_GOLDEN_PRINT").is_some() {
        println!("(\"{name}\", 0x{got:016x}),");
        return;
    }
    assert_eq!(
        got, expected,
        "golden scenario `{name}`: wire bytes drifted (got 0x{got:016x}, \
         pinned 0x{expected:016x}) — encoder output is no longer byte-identical"
    );
}

// ---------------------------------------------------------------------------
// The pins
// ---------------------------------------------------------------------------
//
// Regenerate with OXIDEAV_CINEPAK_GOLDEN_PRINT=1 (see module docs) —
// but in a performance round these numbers must never change.

#[test]
fn golden_intra_single_frame_matrix() {
    // --- default options, single-strip 64x64 intra ------------------
    let rgb64 = rgb_frame(64, 64, 0);
    check(
        "intra/rgb24/64x64/default",
        vec![encode_rgb24(&rgb64, 64, 64, opts_default()).unwrap()],
        GOLDEN_INTRA_DEFAULT_64,
    );

    // --- training-minimal legacy path --------------------------------
    check(
        "intra/rgb24/64x64/legacy-minimal",
        vec![encode_rgb24(&rgb64, 64, 64, opts_legacy_minimal()).unwrap()],
        GOLDEN_INTRA_LEGACY_64,
    );

    // --- 3 strips + vintage_compat on a taller frame -----------------
    let rgb96 = rgb_frame(96, 96, 0);
    let opts_v3 = EncoderOptions {
        strip_count: 3,
        vintage_compat: true,
        ..opts_default()
    };
    check(
        "intra/rgb24/96x96/strips3-vintage",
        vec![encode_rgb24(&rgb96, 96, 96, opts_v3).unwrap()],
        GOLDEN_INTRA_STRIPS3_VINTAGE,
    );

    // --- maximum codebooks, 2 strips ----------------------------------
    let rgb80 = rgb_frame(80, 64, 0);
    let opts_max = EncoderOptions {
        v4_entries: 256,
        v1_entries: 256,
        strip_count: 2,
        ..opts_default()
    };
    check(
        "intra/rgb24/80x64/entries256-strips2",
        vec![encode_rgb24(&rgb80, 80, 64, opts_max).unwrap()],
        GOLDEN_INTRA_MAXBOOK,
    );

    // --- tiny codebooks (deep-quantisation shape) ---------------------
    let opts_tiny = EncoderOptions {
        v4_entries: 8,
        v1_entries: 8,
        skip_threshold: 256.0,
        ..opts_default()
    };
    check(
        "intra/rgb24/64x64/entries8",
        vec![encode_rgb24(&rgb64, 64, 64, opts_tiny).unwrap()],
        GOLDEN_INTRA_TINYBOOK,
    );

    // --- grayscale intra ----------------------------------------------
    let gy = gray_frame(64, 64, 0);
    check(
        "intra/gray8/64x64/default",
        vec![encode_gray8(&gy, 64, 64, opts_default()).unwrap()],
        GOLDEN_INTRA_GRAY8,
    );
}

#[test]
fn golden_picker_tier_matrix() {
    let rgb64 = rgb_frame(64, 64, 0);
    let opts = opts_default();
    check(
        "picker/rgb24/64x64/round6",
        vec![encode_rgb24_round6(&rgb64, 64, 64, opts).unwrap()],
        GOLDEN_PICKER_ROUND6,
    );
    check(
        "picker/rgb24/64x64/round7",
        vec![encode_rgb24_round7(&rgb64, 64, 64, opts).unwrap()],
        GOLDEN_PICKER_ROUND7,
    );
    check(
        "picker/rgb24/64x64/round8",
        vec![encode_rgb24_round8(&rgb64, 64, 64, opts).unwrap()],
        GOLDEN_PICKER_ROUND8,
    );
}

#[test]
fn golden_stateful_gop_matrix() {
    // --- 6-frame RGB GOP: intra, 4x inter, intra (keyframe wrap) ------
    let (w, h) = (96u32, 72u32);
    let mut enc = CinepakEncoder::new().with_keyframe_interval(5);
    let mut frames = Vec::new();
    let mut key_pattern = Vec::new();
    for phase in 0..6 {
        let rgb = rgb_frame(w as usize, h as usize, phase * 2);
        let f = enc
            .encode_frame(&rgb, w, h, opts_default())
            .expect("gop frame");
        key_pattern.push(f.is_keyframe);
        frames.push(f.bytes);
    }
    assert_eq!(
        key_pattern,
        [true, false, false, false, false, true],
        "keyframe cadence drifted"
    );
    check("gop/rgb24/96x72/kf5", frames, GOLDEN_GOP_RGB);

    // --- multi-strip vintage GOP (header-only chunk-omission path) ----
    let mut enc = CinepakEncoder::new().with_keyframe_interval(8);
    let opts_v = EncoderOptions {
        strip_count: 3,
        vintage_compat: true,
        ..opts_default()
    };
    let mut frames = Vec::new();
    for phase in 0..4 {
        let rgb = rgb_frame(w as usize, h as usize, phase);
        frames.push(enc.encode_frame(&rgb, w, h, opts_v).unwrap().bytes);
    }
    check(
        "gop/rgb24/96x72/strips3-vintage",
        frames,
        GOLDEN_GOP_VINTAGE,
    );

    // --- aggressive stale-slot reclamation ----------------------------
    let mut enc = CinepakEncoder::new().with_keyframe_interval(16);
    let opts_stale = EncoderOptions {
        stale_slot_threshold: Some(1),
        ..opts_default()
    };
    let mut frames = Vec::new();
    for phase in 0..6 {
        // Larger pan step so codebook content actually churns.
        let rgb = rgb_frame(64, 48, phase * 7);
        frames.push(enc.encode_frame(&rgb, 64, 48, opts_stale).unwrap().bytes);
    }
    check("gop/rgb24/64x48/stale1", frames, GOLDEN_GOP_STALE);

    // --- grayscale GOP -------------------------------------------------
    let mut enc = CinepakEncoder::new().with_keyframe_interval(6);
    let mut frames = Vec::new();
    for phase in 0..4 {
        let gy = gray_frame(64, 48, phase * 2);
        frames.push(
            enc.encode_frame_gray8(&gy, 64, 48, opts_default())
                .unwrap()
                .bytes,
        );
    }
    check("gop/gray8/64x48/kf6", frames, GOLDEN_GOP_GRAY8);
}

#[test]
fn golden_rate_control_matrix() {
    // Per-frame byte budget with CBR carry-over — the budget grid sweep
    // must keep picking identical candidates.
    let (w, h) = (96u32, 72u32);
    let mut enc = CinepakEncoder::new()
        .with_keyframe_interval(5)
        .with_target_frame_bytes(2600);
    let mut frames = Vec::new();
    for phase in 0..5 {
        let rgb = rgb_frame(w as usize, h as usize, phase * 3);
        frames.push(enc.encode_frame(&rgb, w, h, opts_default()).unwrap().bytes);
    }
    check("rate/rgb24/96x72/budget2600", frames, GOLDEN_RATE_BUDGET);

    // Budget + carry-over cap variant (clamps the surplus path).
    let mut enc = CinepakEncoder::new()
        .with_keyframe_interval(5)
        .with_target_frame_bytes(2000)
        .with_carry_over_cap_bytes(500);
    let mut frames = Vec::new();
    for phase in 0..4 {
        let rgb = rgb_frame(w as usize, h as usize, phase * 3);
        frames.push(enc.encode_frame(&rgb, w, h, opts_default()).unwrap().bytes);
    }
    check(
        "rate/rgb24/96x72/budget2000-cap500",
        frames,
        GOLDEN_RATE_CAP,
    );
}

// ---------------------------------------------------------------------------
// Pinned digests (round 430)
// ---------------------------------------------------------------------------

const GOLDEN_INTRA_DEFAULT_64: u64 = 0x802a_51be_713c_4fae;
const GOLDEN_INTRA_LEGACY_64: u64 = 0xe049_8886_c881_5c45;
const GOLDEN_INTRA_STRIPS3_VINTAGE: u64 = 0xa12a_b8e6_e727_3f33;
const GOLDEN_INTRA_MAXBOOK: u64 = 0xdb41_957c_605b_cdf1;
const GOLDEN_INTRA_TINYBOOK: u64 = 0xb4e7_47f4_4e3e_aa22;
const GOLDEN_INTRA_GRAY8: u64 = 0x5108_0d93_e56f_7ae8;
const GOLDEN_PICKER_ROUND6: u64 = 0x303b_1557_10ef_9a43;
const GOLDEN_PICKER_ROUND7: u64 = 0x6c4b_0071_b3ee_bd72;
const GOLDEN_PICKER_ROUND8: u64 = 0x9cde_8a60_a311_2bf8;
const GOLDEN_GOP_RGB: u64 = 0xcbc6_3d5e_cfe1_f6b0;
const GOLDEN_GOP_VINTAGE: u64 = 0x1047_d12b_eb6d_9112;
const GOLDEN_GOP_STALE: u64 = 0x9be0_b472_7e60_b6ba;
const GOLDEN_GOP_GRAY8: u64 = 0xbc8e_fa57_3480_1d00;
const GOLDEN_RATE_BUDGET: u64 = 0x5b22_1bca_e30a_fc78;
const GOLDEN_RATE_CAP: u64 = 0x7a95_02c7_7349_1cfa;
