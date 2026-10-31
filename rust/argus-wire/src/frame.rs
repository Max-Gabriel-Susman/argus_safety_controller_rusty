//! The telemetry frame: one v3 `argus_frame_packet_t` per feature bin, sent
//! to the host on port 5005.
//!
//! Byte for byte what `argus_send_frame()` in `argus_net.c` builds: the
//! magic, the bin's sample index, its time, version 3, 96 channels, the 96
//! crossing counts and the 96 powers, then the CRC over everything in front
//! of the `crc` field. The arithmetic that produces `t` and the powers is
//! the caller's; this only lays them out.

use core::mem::offset_of;

use zerocopy::IntoBytes;

use crate::crc;
use crate::generated::{ARGUS_FRAME_MAGIC, ARGUS_FRAME_VERSION, ARGUS_MAX_CHANNELS};

pub use crate::generated::argus_frame_packet_t as FramePacket;

/// Channels in every frame. The arrays in [`FramePacket`] have this length
/// because the header says 96; this says the same thing to callers.
pub const CHANNELS: usize = 96;
const _: () = assert!(ARGUS_MAX_CHANNELS == 96);

/// Bytes the CRC covers: [0, 592), everything before the `crc` field.
pub const CRC_SPAN: usize = offset_of!(FramePacket, crc);

// bindgen makes every `#define` a `u32`. These check at build time that the
// version fits its u8 field and the channel count its u16 field.
const _: () = assert!(ARGUS_FRAME_VERSION <= 0xFF);
const _: () = assert!(ARGUS_MAX_CHANNELS <= 0xFFFF);
#[expect(
    clippy::cast_possible_truncation,
    reason = "the asserts above prove each value fits its field"
)]
const VERSION: u8 = ARGUS_FRAME_VERSION as u8;
#[expect(
    clippy::cast_possible_truncation,
    reason = "the asserts above prove each value fits its field"
)]
const CHANNEL_COUNT: u16 = ARGUS_MAX_CHANNELS as u16;

/// Builds the frame for one bin. Send `frame.as_bytes()`: 594 bytes,
/// little-endian as the A9 and the host both are.
///
/// `sample` is the bin's FEATURE_INDEX and `t` its time in seconds;
/// `channels` are the crossing counts and `power` the mean squares, both
/// in channel order.
pub fn encode_frame(
    sample: u32,
    t: f32,
    channels: &[u16; CHANNELS],
    power: &[u32; CHANNELS],
) -> FramePacket {
    // Every field is set here, the reserved byte and the crc to zero, as
    // the C's `= {0}` leaves them before the CRC is filled in.
    let mut frame = FramePacket {
        magic: ARGUS_FRAME_MAGIC,
        sample,
        t,
        version: VERSION,
        reserved: 0,
        channel_count: CHANNEL_COUNT,
        channels: *channels,
        power: *power,
        crc: 0,
    };
    // CRC_SPAN is an offset inside the struct, so the get() always
    // succeeds; the fallback only keeps the code free of a panic path.
    let covered = frame.as_bytes().get(..CRC_SPAN).unwrap_or(&[]);
    frame.crc = crc::checksum(covered);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_the_header() {
        assert_eq!(size_of::<FramePacket>(), 594);
        assert_eq!(CRC_SPAN, 592);
    }

    #[test]
    fn known_frame() {
        let mut channels = [0u16; CHANNELS];
        let mut power = [0u32; CHANNELS];
        for (i, (c, p)) in channels.iter_mut().zip(power.iter_mut()).enumerate() {
            *c = u16::try_from(i).unwrap_or(0);
            *p = u32::try_from(i).unwrap_or(0) * 1000;
        }
        let frame = encode_frame(20, 0.999_6, &channels, &power);
        let bytes = frame.as_bytes();

        assert_eq!(bytes.get(..4), Some(&b"SGRA"[..]), "magic, little-endian");
        assert_eq!(bytes.get(4..8), Some(&20u32.to_le_bytes()[..]));
        assert_eq!(bytes.get(8..12), Some(&0.999_6f32.to_le_bytes()[..]));
        assert_eq!(bytes.get(12..16), Some(&[3, 0, 96, 0][..]));
        assert_eq!(bytes.get(16..18), Some(&[0, 0][..]), "channel 0 count");
        assert_eq!(bytes.get(206..208), Some(&[95, 0][..]), "channel 95 count");
        assert_eq!(bytes.get(588..592), Some(&95_000u32.to_le_bytes()[..]));

        // The CRC sits after the span it covers, little-endian.
        let crc = crc::checksum(bytes.get(..592).unwrap_or(&[]));
        assert_eq!(bytes.get(592..), Some(&crc.to_le_bytes()[..]));
        // CCITT-FALSE has no final XOR, so running the CRC on through its
        // own big-endian value leaves a zero register.
        let residue = crc::Crc16::new()
            .update(bytes.get(..592).unwrap_or(&[]))
            .update(&crc.to_be_bytes())
            .finish();
        assert_eq!(residue, 0);
    }
}
