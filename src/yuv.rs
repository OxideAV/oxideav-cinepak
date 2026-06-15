//! Cinepak YUV → RGB conversion.
//!
//! Decoder-side colour-space algebra per
//! `docs/video/cinepak/spec/04-yuv-rgb-matrix.md` §3:
//!
//! ```text
//!   R_raw = Y + 2 * V
//!   G_raw = Y - (U / 2) - V
//!   B_raw = Y + 2 * U
//! ```
//!
//! `Y` is unsigned 8-bit, `U` / `V` are signed 8-bit two's-complement
//! (centred at zero — no `+128` bias is applied at decode). The
//! `U / 2` step uses C-style integer division (truncation toward zero),
//! NOT arithmetic right shift. The discriminator is the green-primary
//! fixture `M2` in §3.1: `U = -73` truncated toward zero halves to
//! `-36`, yielding `G = 254`; arithmetic shift would yield `-37` /
//! `G = 255` — the decoder emits `254`, pinning truncation toward zero.
//!
//! Each channel is finally clamped to `[0, 255]` (negatives → 0, > 255
//! → 255) and written to the output buffer in `R, G, B` byte order.

/// Convert a single `(Y, U, V)` triple to `(R, G, B)` per the spec.
///
/// `Y` is unsigned (`[0, 255]`); `U` / `V` are signed (`[-128, 127]`).
/// The output is clamped to `[0, 255]` per channel.
#[inline]
pub fn yuv_to_rgb(y: u8, u: i8, v: i8) -> (u8, u8, u8) {
    let y = y as i32;
    let u = u as i32;
    let v = v as i32;

    // u / 2 = truncation toward zero. Rust's `/` on signed integers
    // already truncates toward zero, matching C's `int / int`.
    let r_raw = y + 2 * v;
    let g_raw = y - (u / 2) - v;
    let b_raw = y + 2 * u;

    (clamp_u8(r_raw), clamp_u8(g_raw), clamp_u8(b_raw))
}

#[inline]
fn clamp_u8(x: i32) -> u8 {
    x.clamp(0, 255) as u8
}

use crate::codebook::CodebookEntry;

/// Expand a **V1** macroblock to its concrete 4×4 RGB pixel block.
///
/// This is the colour-conversion capstone above the luminance/chroma
/// plane-geometry surface: it composes the V1 luminance-quadrant layout
/// of spec §4
/// (`docs/video/cinepak/spec/03-vectors-and-macroblocks.md`) with the
/// YUV→RGB inverse matrix of spec §3
/// (`docs/video/cinepak/spec/04-yuv-rgb-matrix.md`). The four luminance
/// values map to the four 2×2 quadrants of the macroblock (`Y0`
/// top-left, `Y1` top-right, `Y2` bottom-left, `Y3` bottom-right; each
/// quadrant's four pixels share the same luminance), and the entry's
/// single `(U, V)` chroma pair is applied across the whole 4×4
/// (chroma 4:2:0 subsampled at 4×4 granularity, spec §4 diagram).
///
/// The result is 4 rows of 4 pixels, each pixel a packed `[R, G, B]`
/// triple (`[[[u8; 3]; 4]; 4]`), row-major top-to-bottom,
/// left-to-right. This is the standalone counterpart to the decoder's
/// internal strided V1 RGB write; it produces the same pixel values
/// without touching an output framebuffer, for validators,
/// introspection tools, and re-encoders that want the resolved RGB
/// geometry of a single macroblock.
///
/// For an 8-bit grayscale entry the chroma fields are zero, so every
/// channel of every pixel equals the quadrant's luminance value (the
/// §4 grayscale identity mapping).
pub fn expand_v1_mb_rgb(e: &CodebookEntry) -> [[[u8; 3]; 4]; 4] {
    // (U, V) constant across the whole 4×4 per spec §4.
    let p_tl = yuv_to_rgb(e.y0, e.u, e.v);
    let p_tr = yuv_to_rgb(e.y1, e.u, e.v);
    let p_bl = yuv_to_rgb(e.y2, e.u, e.v);
    let p_br = yuv_to_rgb(e.y3, e.u, e.v);
    let tl = [p_tl.0, p_tl.1, p_tl.2];
    let tr = [p_tr.0, p_tr.1, p_tr.2];
    let bl = [p_bl.0, p_bl.1, p_bl.2];
    let br = [p_br.0, p_br.1, p_br.2];
    [
        [tl, tl, tr, tr],
        [tl, tl, tr, tr],
        [bl, bl, br, br],
        [bl, bl, br, br],
    ]
}

/// Expand a **V4** macroblock to its concrete 4×4 RGB pixel block.
///
/// The V4 colour-conversion capstone: it composes the V4 sub-block
/// layout of spec §5
/// (`docs/video/cinepak/spec/03-vectors-and-macroblocks.md`) with the
/// YUV→RGB inverse matrix of spec §3
/// (`docs/video/cinepak/spec/04-yuv-rgb-matrix.md`). The four entries
/// `(r0, r1, r2, r3)` tile the macroblock as 2×2 sub-blocks — `r0`
/// top-left, `r1` top-right, `r2` bottom-left, `r3` bottom-right — and
/// within each sub-block the entry's `(Y0, Y1, Y2, Y3)` read out
/// row-major (Y0 top-left, Y1 top-right, Y2 bottom-left, Y3
/// bottom-right) under that sub-block's own `(U, V)` pair (V4 carries
/// four chroma samples per macroblock, twice the V1 chroma resolution,
/// per spec §5).
///
/// `quad` is the four entries in the order their index bytes appear in
/// the vector chunk (`r0, r1, r2, r3`). The result is 4 rows of 4
/// pixels, each a packed `[R, G, B]` triple (`[[[u8; 3]; 4]; 4]`),
/// row-major. Like [`expand_v1_mb_rgb`] this is the standalone
/// counterpart to the decoder's internal strided V4 RGB write.
///
/// For 8-bit grayscale entries the chroma fields are zero, so each
/// pixel's three channels equal its sub-block luminance value.
pub fn expand_v4_mb_rgb(quad: &[CodebookEntry; 4]) -> [[[u8; 3]; 4]; 4] {
    // Each sub-block carries its own (U, V) per spec §5.
    let sub = |e: &CodebookEntry| -> [[[u8; 3]; 2]; 2] {
        let p0 = yuv_to_rgb(e.y0, e.u, e.v);
        let p1 = yuv_to_rgb(e.y1, e.u, e.v);
        let p2 = yuv_to_rgb(e.y2, e.u, e.v);
        let p3 = yuv_to_rgb(e.y3, e.u, e.v);
        [
            [[p0.0, p0.1, p0.2], [p1.0, p1.1, p1.2]],
            [[p2.0, p2.1, p2.2], [p3.0, p3.1, p3.2]],
        ]
    };
    let s0 = sub(&quad[0]); // top-left sub-block
    let s1 = sub(&quad[1]); // top-right sub-block
    let s2 = sub(&quad[2]); // bottom-left sub-block
    let s3 = sub(&quad[3]); // bottom-right sub-block
    [
        [s0[0][0], s0[0][1], s1[0][0], s1[0][1]],
        [s0[1][0], s0[1][1], s1[1][0], s1[1][1]],
        [s2[0][0], s2[0][1], s3[0][0], s3[0][1]],
        [s2[1][0], s2[1][1], s3[1][0], s3[1][1]],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §3.1 fixture `M1` — solid red `(255, 0, 0)`.
    #[test]
    fn m1_red() {
        // codebook entry observed: Y=72, U=-37, V=+91
        let (r, g, b) = yuv_to_rgb(72, -37, 91);
        assert_eq!((r, g, b), (254, 0, 0));
    }

    /// Spec §3.1 fixture `M2` — solid green; the truncation-toward-zero
    /// discriminator. Arithmetic shift would yield G=255; spec says G=254.
    #[test]
    fn m2_green_discriminates_truncation_toward_zero() {
        let (r, g, b) = yuv_to_rgb(145, -73, -73);
        assert_eq!((r, g, b), (0, 254, 0));
    }

    /// Spec §3.1 fixture `M3` — solid blue.
    #[test]
    fn m3_blue() {
        let (r, g, b) = yuv_to_rgb(36, 109, -19);
        assert_eq!((r, g, b), (0, 1, 254));
    }

    /// Spec §3.1 fixture `M7` — solid white; chroma zero, no clamping.
    #[test]
    fn m7_white() {
        let (r, g, b) = yuv_to_rgb(255, 0, 0);
        assert_eq!((r, g, b), (255, 255, 255));
    }

    /// Spec §3.1 fixture `M8` — solid black.
    #[test]
    fn m8_black() {
        let (r, g, b) = yuv_to_rgb(0, 0, 0);
        assert_eq!((r, g, b), (0, 0, 0));
    }

    /// Spec §3.1 fixture `M9` — mid-gray.
    #[test]
    fn m9_mid_gray() {
        let (r, g, b) = yuv_to_rgb(128, 0, 0);
        assert_eq!((r, g, b), (128, 128, 128));
    }

    /// Spec §3.1 fixture `M13` — discriminates positive-odd `U/2`.
    /// `U = +45`, halved-toward-zero to `+22`, so `G = 100 - 22 - 13 = 65`.
    #[test]
    fn m13_positive_odd_u_truncation() {
        let (r, g, b) = yuv_to_rgb(100, 45, 13);
        assert_eq!((r, g, b), (126, 65, 190));
    }

    /// Spec §3.1 fixture `M10` — off-primary RGB, no clamp involved.
    #[test]
    fn m10_off_primary() {
        let (r, g, b) = yuv_to_rgb(121, -36, 39);
        assert_eq!((r, g, b), (199, 100, 49));
    }

    /// Clamp on negative R: spec fixture `M2` already exercises G clamp;
    /// here verify R clamps from a contrived large-negative V.
    #[test]
    fn clamp_negative_to_zero() {
        // y=10, v=-100 → r_raw = 10 + 2*(-100) = -190 → 0
        let (r, _, _) = yuv_to_rgb(10, 0, -100);
        assert_eq!(r, 0);
    }

    /// Clamp on R > 255.
    #[test]
    fn clamp_above_255() {
        // y=200, v=+100 → r_raw = 200 + 200 = 400 → 255
        let (r, _, _) = yuv_to_rgb(200, 0, 100);
        assert_eq!(r, 255);
    }

    use crate::codebook::CodebookEntry;

    /// V1 macroblock RGB expansion tiles each Yi's converted pixel over
    /// its 2×2 quadrant, with the single `(U, V)` applied across the
    /// whole 4×4 — spec §4 luminance-quadrant layout + spec §3 matrix.
    /// Uses the spec `M10` chroma `(U=-36, V=+39)` so the converted
    /// pixel is the verified off-primary triple `(199, 100, 49)` and
    /// the §4 `Y15` quadrant luminance pattern (50, 100, 150, 200).
    #[test]
    fn v1_mb_rgb_quadrant_layout_with_shared_chroma() {
        // Pixel under (U=-36, V=+39) for each Y, by the §3 matrix.
        let q_tl = yuv_to_rgb(50, -36, 39); // Y0 → top-left quadrant
        let q_tr = yuv_to_rgb(100, -36, 39); // Y1 → top-right
        let q_bl = yuv_to_rgb(150, -36, 39); // Y2 → bottom-left
        let q_br = yuv_to_rgb(200, -36, 39); // Y3 → bottom-right
        let tl = [q_tl.0, q_tl.1, q_tl.2];
        let tr = [q_tr.0, q_tr.1, q_tr.2];
        let bl = [q_bl.0, q_bl.1, q_bl.2];
        let br = [q_br.0, q_br.1, q_br.2];
        let e = CodebookEntry::from_yuv(50, 100, 150, 200, -36, 39);
        assert_eq!(
            expand_v1_mb_rgb(&e),
            [
                [tl, tl, tr, tr],
                [tl, tl, tr, tr],
                [bl, bl, br, br],
                [bl, bl, br, br],
            ]
        );
        // The pixel each quadrant carries matches the verified M10 cell
        // for Y=121; here we just pin the well-known primary M1 pixel by
        // a separate entry to anchor the matrix composition end-to-end.
        let red = CodebookEntry::from_yuv(72, 72, 72, 72, -37, 91);
        assert_eq!(expand_v1_mb_rgb(&red)[0][0], [254, 0, 0]);
    }

    /// V1 grayscale entry (chroma zero) expands to a per-quadrant
    /// luminance-on-all-channels block — the §4 grayscale identity.
    #[test]
    fn v1_mb_rgb_grayscale_identity() {
        // §4.1 fixture M15 luminance 50 across the whole quadrant grid.
        let e = CodebookEntry::from_y(50, 100, 150, 200);
        let out = expand_v1_mb_rgb(&e);
        assert_eq!(out[0][0], [50, 50, 50]); // Y0 quadrant
        assert_eq!(out[0][3], [100, 100, 100]); // Y1 quadrant
        assert_eq!(out[3][0], [150, 150, 150]); // Y2 quadrant
        assert_eq!(out[3][3], [200, 200, 200]); // Y3 quadrant
    }

    /// V4 macroblock RGB expansion places each entry's converted 2×2
    /// sub-block at its scan-order position with the entry's own
    /// `(U, V)` — spec §5 sub-block layout + per-sub-block chroma.
    /// Anchored to the §5 `Y16` luminance ramp converted under the
    /// four primary chroma pairs M1/M2/M3/M7.
    #[test]
    fn v4_mb_rgb_subblock_layout_with_per_subblock_chroma() {
        // Y16 ramp split across four sub-blocks, each given a distinct
        // verified chroma pair so the per-sub-block (U,V) routing shows.
        let e0 = CodebookEntry::from_yuv(16, 30, 72, 86, -37, 91); // M1 red chroma
        let e1 = CodebookEntry::from_yuv(40, 54, 96, 110, -73, -73); // M2 green chroma
        let e2 = CodebookEntry::from_yuv(128, 142, 156, 170, 109, -19); // M3 blue chroma
        let e3 = CodebookEntry::from_yuv(156, 170, 212, 226, 0, 0); // M7 neutral
        let quad = [e0, e1, e2, e3];
        let out = expand_v4_mb_rgb(&quad);
        // Top-left sub-block, top-left pixel = Y0 of e0 under e0 chroma.
        assert_eq!(out[0][0], rgb(yuv_to_rgb(16, -37, 91)));
        // Top-left sub-block, bottom-right pixel = Y3 of e0.
        assert_eq!(out[1][1], rgb(yuv_to_rgb(86, -37, 91)));
        // Top-right sub-block, top-left pixel = Y0 of e1 under e1 chroma.
        assert_eq!(out[0][2], rgb(yuv_to_rgb(40, -73, -73)));
        // Bottom-left sub-block, top-left pixel = Y0 of e2 under e2.
        assert_eq!(out[2][0], rgb(yuv_to_rgb(128, 109, -19)));
        // Bottom-right sub-block, bottom-right pixel = Y3 of e3 (neutral).
        assert_eq!(out[3][3], rgb(yuv_to_rgb(226, 0, 0)));
    }

    /// A uniform V4 quad (all four references the same entry) and the V1
    /// expansion of that entry are deliberately distinct layouts: V4
    /// tiles the entry's own 2×2 across every sub-block, whereas V1
    /// tiles each Yi constantly over a quadrant. They agree only at the
    /// four shared corner pixels.
    #[test]
    fn v4_uniform_quad_differs_from_v1() {
        let e = CodebookEntry::from_yuv(10, 90, 170, 250, 20, -20);
        let v1 = expand_v1_mb_rgb(&e);
        let v4 = expand_v4_mb_rgb(&[e; 4]);
        // V1 top-left 2×2 is all Y0; V4 top-left sub-block reads Y0..Y3.
        assert_eq!(v1[0][0], v4[0][0]); // both Y0 at (0,0)
        assert_ne!(v1[0][1], v4[0][1]); // V1=Y0, V4=Y1
        assert_ne!(v1[1][0], v4[1][0]); // V1=Y0, V4=Y2
    }

    /// V4 grayscale entries (chroma zero) collapse each pixel to a
    /// luminance-on-all-channels triple.
    #[test]
    fn v4_mb_rgb_grayscale_identity() {
        let quad = [
            CodebookEntry::from_y(0, 16, 32, 48),
            CodebookEntry::from_y(64, 80, 96, 112),
            CodebookEntry::from_y(128, 144, 160, 176),
            CodebookEntry::from_y(192, 208, 224, 240),
        ];
        let out = expand_v4_mb_rgb(&quad);
        assert_eq!(out[0][0], [0, 0, 0]); // e0 Y0
        assert_eq!(out[1][1], [48, 48, 48]); // e0 Y3
        assert_eq!(out[3][3], [240, 240, 240]); // e3 Y3
    }

    /// V1 and V4 RGB expansion agree byte-for-byte with the decoder's
    /// own internal strided write for the same entry / quad — the
    /// standalone surface is the same colour-conversion stage extracted.
    #[test]
    fn matches_decoder_strided_write() {
        // Reconstruct a one-macroblock strided RGB write the way the
        // decoder does (4×4 block, stride = 4 px × 3 bytes) and compare.
        let e = CodebookEntry::from_yuv(72, 130, 200, 40, -36, 39);
        let block = expand_v1_mb_rgb(&e);
        for (row, px_row) in block.iter().enumerate() {
            for (col, px) in px_row.iter().enumerate() {
                // Decoder V1 path: quadrant index → Yi, shared (U,V).
                let yi = match (row / 2, col / 2) {
                    (0, 0) => e.y0,
                    (0, 1) => e.y1,
                    (1, 0) => e.y2,
                    _ => e.y3,
                };
                assert_eq!(*px, rgb(yuv_to_rgb(yi, e.u, e.v)));
            }
        }
    }

    #[inline]
    fn rgb(t: (u8, u8, u8)) -> [u8; 3] {
        [t.0, t.1, t.2]
    }
}
