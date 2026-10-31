//! The Rust frame encoder against the packing code of `argus_send_frame()`.
//!
//! Random frames, the same values handed to both; all 594 bytes must match.
//! The `t` values include the awkward ones (zero of either sign, the
//! infinities, a NaN, the smallest subnormal), since the encoder must carry
//! a float's bits through untouched.

use argus_difftest::Failure;
use argus_difftest::c::pack_frame;
use argus_difftest::rng::{Rng, SEED_VAR, run_seed};
use argus_wire::frame::{CHANNELS, encode_frame};
use zerocopy::IntoBytes;

/// Frames per run.
const FRAMES: u32 = 10_000;

/// Times worth a frame of their own.
const SPECIAL_T: [f32; 7] = [
    0.0,
    -0.0,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::NAN,
    f32::MIN_POSITIVE,
    f32::MAX,
];

#[test]
fn c_and_rust_frames_agree() -> Result<(), Failure> {
    let seed = run_seed()?;
    println!("frame difftest: {FRAMES} frames, {SEED_VAR}={seed:#x}");
    let mut rng = Rng::new(seed);

    for i in 0..FRAMES {
        let sample = rng.next_u32();
        let pick = usize::try_from(rng.below(40)).unwrap_or(usize::MAX);
        // Otherwise any bit pattern that is a number: a NaN's payload is
        // not something either side promises to keep.
        let t = SPECIAL_T.get(pick).copied().unwrap_or_else(|| {
            let t = f32::from_bits(rng.next_u32());
            if t.is_nan() { 1.0 } else { t }
        });
        let mut channels = [0u16; CHANNELS];
        let mut power = [0u32; CHANNELS];
        for (c, p) in channels.iter_mut().zip(power.iter_mut()) {
            *c = rng.range_u16(0, u16::MAX);
            *p = rng.next_u32();
        }

        let c = pack_frame(sample, t, &channels, &power);
        let rust = encode_frame(sample, t, &channels, &power);
        if c.as_slice() != rust.as_bytes() {
            let at = c.iter().zip(rust.as_bytes()).position(|(a, b)| a != b);
            return Err(Failure(format!(
                "{SEED_VAR}={seed:#x}, frame {i}: sample {sample}, t {t} ({:#010x}): \
                 bytes differ from {at:?}",
                t.to_bits()
            )));
        }
    }
    Ok(())
}
