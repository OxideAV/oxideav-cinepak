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
}
