//! The telemetry frame each bin becomes, and the unsent-frame count.

use argus_fw_core::acq::{self, Features};
use argus_fw_core::lines::TelemetryUnsent;
use argus_fw_core::telemetry::{self, Unsent, UnsentCounter};
use argus_wire::frame::encode_frame;

#[test]
fn frame_carries_the_bin() {
    let mut features = Features::new(1858);
    // Channel 14: count 7, sum 2^32 + 1500 * 3 = power 2863314, the
    // 48-bit sum divided by 1500 with the remainder dropped.
    features.set(14, 0x0007_0001, 1500 * 3);
    // Channel 75: count 0xFFFF, the largest sum the words can carry.
    features.set(75, 0xFFFF_FFFF, 0xFFFF_FFFF);

    let frame = telemetry::frame(&features);
    // Copied out of the packed struct before they are compared.
    let (magic, sample, t, version, count) = (
        frame.magic,
        frame.sample,
        frame.t,
        frame.version,
        frame.channel_count,
    );
    assert_eq!(magic, 0x4152_4753, "\"SGRA\"");
    assert_eq!(sample, 1858, "the sample is FEATURE_INDEX");
    assert_eq!(t.to_bits(), acq::bin_time(1858).to_bits());
    assert_eq!((version, count), (3, 96));

    let (channels, power) = (frame.channels, frame.power);
    // The C's (uint32_t)(sum / 1500): the low 32 bits of the quotient.
    let power_of = |sum: u64| u32::try_from((sum / 1500) & 0xFFFF_FFFF).ok();
    assert_eq!(channels.get(14).copied(), Some(7));
    assert_eq!(power.get(14).copied(), power_of((1 << 32) + 4500));
    assert_eq!(power.get(14).copied(), Some(2_863_314));
    assert_eq!(channels.get(75).copied(), Some(0xFFFF));
    assert_eq!(power.get(75).copied(), power_of((1 << 48) - 1));
    assert_eq!(power.get(75).copied(), Some(2_966_390_745));
    assert_eq!(channels.first().copied(), Some(0));

    // Everything else, the CRC included, is the encoder's.
    let expected = encode_frame(1858, acq::bin_time(1858), &features.counts, &features.power);
    let (crc, expected_crc) = (frame.crc, expected.crc);
    assert_eq!(crc, expected_crc);
}

#[test]
fn unsent_frames_are_reported_once_per_run() {
    let mut unsent = UnsentCounter::new();
    // Nothing unsent: nothing to say.
    assert_eq!(unsent.sent(), None);

    // The link is down for three bins, then comes up.
    for _ in 0..3 {
        unsent.unsent(Unsent::LinkDown);
    }
    assert_eq!(
        unsent.sent(),
        Some(TelemetryUnsent {
            link_down: 3,
            refused: 0
        })
    );
    assert_eq!(unsent.sent(), None, "reported once");

    // A refusal later: the totals run from boot.
    unsent.unsent(Unsent::Refused);
    assert_eq!(
        unsent.sent(),
        Some(TelemetryUnsent {
            link_down: 3,
            refused: 1
        })
    );
    assert_eq!(unsent.sent(), None);
}
