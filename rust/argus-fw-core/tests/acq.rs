//! The register decoding of `argus_acq.h` and the checks of `main.c`, with
//! the expected values worked out from the C expressions.

use argus_fw_core::acq::{self, ReplayStatus, reg};

#[test]
fn register_offsets() {
    assert_eq!(reg::CTRL, 0x000);
    assert_eq!(reg::STATUS, 0x004);
    assert_eq!(reg::FRAME_INDEX, 0x008);
    assert_eq!(reg::ID, 0x00C);
    assert_eq!(reg::REPLAY_STATUS, 0x010);
    assert_eq!(reg::REPLAY_ACK, 0x014);
    assert_eq!(reg::FEATURE_INDEX, 0x018);
    assert_eq!(reg::FEAT_DROPPED, 0x01C);
    // From argus_acq_axi.vhd; argus_acq.h predates ACQ4.
    assert_eq!(reg::LANE_SRC, 0x020);
    assert_eq!(reg::MISO_DELAY, 0x024);
    assert_eq!(reg::frame(0), 0x100);
    assert_eq!(reg::frame(95), 0x27C);
    assert_eq!(reg::feature_lo(0), 0x400);
    assert_eq!(reg::feature_hi(0), 0x404);
    assert_eq!(reg::feature_lo(95), 0x6F8);
    assert_eq!(reg::feature_hi(95), 0x6FC);
    // The last register lies inside the 4 KB block.
    assert!(reg::feature_hi(95) < 0x1000);
}

#[test]
fn bram_layout() {
    use acq::bram;
    assert_eq!(bram::BASE, 0x4000_0000);
    assert_eq!(bram::SIZE, 0x1_0000);
    // ARGUS_BRAM_HALF_BYTES: 147 samples x 96 channels x 2 bytes.
    assert_eq!(bram::HALF_BYTES, 28_224);
    // Half 1's offset, as the gateware expects it. That the two halves fit
    // and are whole cache lines is asserted at compile time in the crate.
    assert_eq!(bram::HALF_BYTES, 0x6E40);
}

#[test]
fn accepted_fabric_ids() {
    assert!(acq::is_accepted_id(0x4143_5133)); // "ACQ3"
    assert!(acq::is_accepted_id(0x4143_5134)); // "ACQ4"
    assert_eq!(&acq::ID_ACQ3.to_be_bytes(), b"ACQ3");
    assert_eq!(&acq::ID_ACQ4.to_be_bytes(), b"ACQ4");
    for id in [0, 0x4143_5132, 0x4143_5135, 0xDEAD_BEEF, u32::MAX] {
        assert!(!acq::is_accepted_id(id), "{id:08x}");
    }
}

#[test]
fn feature_words() {
    // ARGUS_ACQ_FEAT_COUNT(hi) = (uint16_t)(hi >> 16)
    assert_eq!(acq::feature_count(0x0007_ABCD), 7);
    assert_eq!(acq::feature_count(0xFFFF_0000), 0xFFFF);
    // ARGUS_ACQ_FEAT_SUM(hi, lo) = ((hi & 0xFFFF) << 32) | lo; the count
    // bits do not leak into the sum.
    assert_eq!(acq::feature_sum(0x0007_ABCD, 0x1234_5678), 0xABCD_1234_5678);
    assert_eq!(acq::feature_sum(0xFFFF_0000, 42), 42);
    // power = (uint32_t)(sum / 1500), integer division.
    assert_eq!(acq::feature_power(0, 10_837_500), 7225);
    assert_eq!(acq::feature_power(0, 10_838_999), 7225);
    assert_eq!(acq::feature_power(0, 10_839_000), 7226);
    assert_eq!(acq::feature_power(0, 1499), 0);
    assert_eq!(acq::feature_power(0x0001_0000, 0), 0); // count only
    // A sum past 32 bits: 0x1_0000_0000 / 1500 = 2863311.
    assert_eq!(acq::feature_power(1, 0), 2_863_311);
    // The largest sum the fields hold, truncated to 32 bits as the C's
    // cast does: (2^48 - 1) / 1500 = 187649984473, mod 2^32 = 2966390745.
    assert_eq!(acq::feature_power(0xFFFF, 0xFFFF_FFFF), 2_966_390_745);
}

#[test]
fn features_decode_each_channel() {
    let mut features = acq::Features::new(12);
    // Channel 3: count 7, sum 0xABCD_1234_5678.
    features.set(3, 0x0007_ABCD, 0x1234_5678);
    assert_eq!(features.bin, 12);
    assert_eq!(
        features.channel(3),
        (7, acq::feature_power(0x0007_ABCD, 0x1234_5678))
    );
    assert_eq!(features.channel(2), (0, 0));
    // Past the last channel: ignored on the way in, zeros on the way out.
    features.set(96, 0xFFFF_FFFF, 0xFFFF_FFFF);
    assert_eq!(features.channel(96), (0, 0));
    assert_eq!(features, {
        let mut f = acq::Features::new(12);
        f.set(3, 0x0007_ABCD, 0x1234_5678);
        f
    });
}

#[test]
fn bin_counter_follows_the_main_loop() {
    let mut bins = acq::BinCounter::new();
    // last_bin starts at 0: bin 0 is not new, bin 1 is.
    assert!(!bins.is_new(0));
    assert!(bins.is_new(1));
    assert!(!bins.read(1));
    assert!(!bins.is_new(1));
    // A hold that never took effect: skipped, and the index it read then
    // is taken as seen.
    bins.skipped(3);
    assert!(!bins.is_new(3));
    assert_eq!((bins.read_count(), bins.skipped_count()), (1, 1));
    // The report falls on every 100th bin read, skips not counted.
    let due: Vec<u32> = (4..400).filter(|&bin| bins.read(bin)).collect();
    assert_eq!(due, [102, 202, 302]);
    assert_eq!(bins.read_count(), 397);
}

#[test]
fn bin_time_rounds_as_the_c_does() {
    // (float)bin * ((float)1500 / 30012.0f), in single precision.
    let step = 1500.0f32 / 30012.0f32;
    for bin in [0, 1, 20, 1858, 100_000, 16_777_217, u32::MAX] {
        assert_eq!(acq::bin_time(bin).to_bits(), (bin as f32 * step).to_bits());
    }
    assert_eq!(acq::bin_time(0), 0.0);
    // 20 bins is one second, near enough.
    assert!((acq::bin_time(20) - 0.9996).abs() < 1e-4);
}

#[test]
fn frame_words() {
    // ((n / 32) << 14) | ((n % 32) << 8) | idx
    assert_eq!(acq::frame_word(0, 0x33), 0x0033);
    assert_eq!(acq::frame_word(1, 0x33), 0x0133);
    assert_eq!(acq::frame_word(31, 0x00), 0x1F00);
    assert_eq!(acq::frame_word(32, 0x00), 0x4000);
    assert_eq!(acq::frame_word(64, 0xFF), 0x80FF);
    assert_eq!(acq::frame_word(95, 0x33), 0x9F33);
}

#[test]
fn frame_check() {
    let mut frame: [u16; 96] =
        core::array::from_fn(|n| acq::frame_word(u16::try_from(n).unwrap_or(0), 0x5A));
    let check = acq::check_frame(&frame);
    assert_eq!((check.idx, check.bad), (0x5A, 0));

    // Channel 0 sets the index for the whole frame: change it and every
    // other word is wrong.
    frame[0] = 0x0011;
    let check = acq::check_frame(&frame);
    assert_eq!((check.idx, check.bad), (0x11, 95));

    // Channel 0 with the wrong chip bits is itself bad.
    frame[0] = 0x4011;
    let check = acq::check_frame(&frame);
    assert_eq!((check.idx, check.bad), (0x11, 96));

    // The unmapped-read pattern everywhere.
    let check = acq::check_frame(&[0xBEEF; 96]);
    assert_eq!((check.idx, check.bad), (0xEF, 96));
}

#[test]
fn expected_index() {
    // (fi - 1u) & 0xFFu
    assert_eq!(acq::expected_index(4660), 0x33);
    assert_eq!(acq::expected_index(1), 0x00);
    assert_eq!(acq::expected_index(256), 0xFF);
    assert_eq!(acq::expected_index(257), 0x00);
    assert_eq!(acq::expected_index(0), 0xFF);
}

#[test]
fn replay_status() {
    let rs = ReplayStatus(0);
    assert_eq!((rs.play_half(), rs.row()), (0, 0));
    assert!(!rs.consumed0() && !rs.consumed1() && !rs.underrun());

    let rs = ReplayStatus(1 | 1 << 1 | 146 << 8);
    assert_eq!((rs.play_half(), rs.row()), (1, 146));
    assert!(rs.consumed0() && !rs.consumed1() && !rs.underrun());

    let rs = ReplayStatus(1 << 2 | 1 << 3 | 0xFF00_0000);
    assert_eq!((rs.play_half(), rs.row()), (0, 0));
    assert!(!rs.consumed0() && rs.consumed1() && rs.underrun());

    // Bits 23:8, sixteen of them.
    assert_eq!(ReplayStatus(0x00FF_FF00).row(), 0xFFFF);
}

#[test]
fn control_and_status_bits() {
    use acq::{ctrl, replay_ack, status};
    assert_eq!(
        [
            ctrl::ENABLE,
            ctrl::SOFT_RESET,
            ctrl::EXT_MODE,
            ctrl::HOLD,
            ctrl::FEAT_HOLD
        ],
        [1, 2, 4, 8, 16]
    );
    assert_eq!(
        [
            status::READY,
            status::OVERRUN,
            status::HELD,
            status::FEAT_HELD
        ],
        [1, 2, 4, 8]
    );
    assert_eq!(
        [
            replay_ack::CONSUMED0,
            replay_ack::CONSUMED1,
            replay_ack::UNDERRUN
        ],
        [1, 2, 4]
    );
}
