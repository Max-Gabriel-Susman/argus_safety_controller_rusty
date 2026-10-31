//! The RTL8211E driver against a fake MDIO bus that holds a register file
//! per address and records every read and write.
//!
//! The identifier values are the RTL8211E's (PHYID1 0x001C, PHYID2 0xC915,
//! revision 5); the PHYSR values are built from the datasheet's fields.

use std::collections::{HashMap, HashSet};

use argus_fw_core::phy::{
    self, ADVERTISE, ADVERTISE_1000, Duplex, Link, LinkChange, LinkWatch, Mdio, PhyId, Rtl8211e,
    Speed, bmcr, bmsr, physr, reg,
};

const RTL8211E: PhyId = PhyId {
    id1: 0x001C,
    id2: 0xC915,
};

/// A bus of PHYs at the addresses in `present`, each with the registers in
/// `regs` (any register not set reads 0). An address with no PHY reads
/// 0xFFFF, as the pulled-up MDIO line does. BMCR's reset bit stays set for
/// `reset_reads` reads after it is written, then clears, as the PHY's does
/// when its reset is over.
#[derive(Default)]
struct FakeMdio {
    regs: HashMap<(u8, u8), u16>,
    present: HashSet<u8>,
    writes: Vec<(u8, u8, u16)>,
    reads: Vec<(u8, u8)>,
    reset_reads: u32,
    reset_left: u32,
}

impl FakeMdio {
    /// The bus with a PHY of identifier `id` at `addr`.
    fn with_phy(addr: u8, id: PhyId) -> Self {
        let mut bus = Self::default();
        bus.place(addr, id);
        bus
    }

    fn place(&mut self, addr: u8, id: PhyId) {
        self.present.insert(addr);
        self.set(addr, reg::PHYID1, id.id1);
        self.set(addr, reg::PHYID2, id.id2);
    }

    fn set(&mut self, addr: u8, r: u8, value: u16) {
        self.regs.insert((addr, r), value);
    }

    /// The reads since the last call, oldest first.
    fn take_reads(&mut self) -> Vec<(u8, u8)> {
        std::mem::take(&mut self.reads)
    }
}

impl Mdio for FakeMdio {
    fn read(&mut self, phy: u8, r: u8) -> u16 {
        self.reads.push((phy, r));
        if !self.present.contains(&phy) {
            return 0xFFFF;
        }
        let value = self.regs.entry((phy, r)).or_insert(0);
        if r == reg::BMCR && *value & bmcr::RESET != 0 {
            if self.reset_left == 0 {
                *value &= !bmcr::RESET;
            } else {
                self.reset_left -= 1;
            }
        }
        *value
    }

    fn write(&mut self, phy: u8, r: u8, value: u16) {
        self.writes.push((phy, r, value));
        if r == reg::BMCR && value & bmcr::RESET != 0 {
            self.reset_left = self.reset_reads;
        }
        if self.present.contains(&phy) {
            self.set(phy, r, value);
        }
    }
}

type Result = core::result::Result<(), &'static str>;

fn found(bus: &mut FakeMdio) -> core::result::Result<Rtl8211e, &'static str> {
    Rtl8211e::probe(bus).ok_or("no RTL8211E on the bus")
}

#[test]
fn register_numbers() {
    assert_eq!(reg::BMCR, 0x00);
    assert_eq!(reg::BMSR, 0x01);
    assert_eq!(reg::PHYID1, 0x02);
    assert_eq!(reg::PHYID2, 0x03);
    assert_eq!(reg::ANAR, 0x04);
    assert_eq!(reg::GBCR, 0x09);
    assert_eq!(reg::PHYSR, 0x11);
}

#[test]
fn identifier_fields() {
    // Realtek's OUI is 00-E0-4C; in the bit-reversed IEEE order the
    // registers use, bits 3 to 24 of it assemble to 0x000732.
    assert_eq!(RTL8211E.oui(), phy::REALTEK_OUI);
    assert_eq!(RTL8211E.model(), phy::RTL8211_MODEL);
    assert_eq!(RTL8211E.revision(), phy::RTL8211E_REVISION);
    assert!(RTL8211E.is_rtl8211e());
    // The same arithmetic gives the Zedboard BSP's Marvell 88E1518 values
    // (OUI 0x005043, model 0b011101) from that PHY's registers.
    let marvell = PhyId {
        id1: 0x0141,
        id2: 0x0DD1,
    };
    assert_eq!(marvell.oui(), 0x00_5043);
    assert_eq!(marvell.model(), 0b01_1101);
    assert!(!marvell.is_rtl8211e());
    // The other RTL8211s share the model number; the revision tells them
    // apart. The F (0xC916) and the B (0xC912) are not an E.
    for id2 in [0xC916, 0xC912] {
        let other = PhyId { id1: 0x001C, id2 };
        assert_eq!(other.oui(), phy::REALTEK_OUI);
        assert_eq!(other.model(), phy::RTL8211_MODEL);
        assert!(!other.is_rtl8211e());
    }
}

#[test]
fn probe_finds_the_phy_at_its_address() -> Result {
    let mut bus = FakeMdio::with_phy(1, RTL8211E);
    let phy = found(&mut bus)?;
    assert_eq!(phy.addr(), 1);
    assert_eq!(phy.id(), RTL8211E);
    // It stops at the first match and writes nothing.
    assert_eq!(bus.reads, [(1, reg::PHYID1), (1, reg::PHYID2)]);
    assert!(bus.writes.is_empty());
    Ok(())
}

#[test]
fn probe_prefers_the_strapped_address_to_broadcast() -> Result {
    // The RTL8211E answers at 0 as well as at its own address.
    let mut bus = FakeMdio::with_phy(0, RTL8211E);
    bus.place(7, RTL8211E);
    assert_eq!(found(&mut bus)?.addr(), 7);
    Ok(())
}

#[test]
fn probe_falls_back_to_address_0() -> Result {
    let mut bus = FakeMdio::with_phy(0, RTL8211E);
    assert_eq!(found(&mut bus)?.addr(), 0);
    // Every other address was tried first, in order, ID1 then ID2.
    let expected: Vec<(u8, u8)> = (1..32)
        .chain([0])
        .flat_map(|a| [(a, reg::PHYID1), (a, reg::PHYID2)])
        .collect();
    assert_eq!(bus.reads, expected);
    Ok(())
}

#[test]
fn probe_rejects_an_empty_bus_and_other_phys() {
    let mut bus = FakeMdio::default();
    assert_eq!(Rtl8211e::probe(&mut bus), None);
    // All 32 addresses were tried; each read 0xFFFF.
    assert_eq!(bus.reads.len(), 64);

    let marvell = PhyId {
        id1: 0x0141,
        id2: 0x0DD1,
    };
    let mut bus = FakeMdio::with_phy(1, marvell);
    assert_eq!(Rtl8211e::probe(&mut bus), None);

    // A bus held low reads 0x0000 everywhere: no PHY either.
    let mut bus = FakeMdio {
        present: (0..32).collect(),
        ..FakeMdio::default()
    };
    assert_eq!(Rtl8211e::probe(&mut bus), None);
}

#[test]
fn soft_reset_is_polled_not_waited_for() -> Result {
    let mut bus = FakeMdio::with_phy(1, RTL8211E);
    bus.reset_reads = 3;
    let phy = found(&mut bus)?;
    bus.take_reads();

    phy.start_reset(&mut bus);
    assert_eq!(bus.writes, [(1, reg::BMCR, bmcr::RESET)]);
    // Starting the reset reads nothing.
    assert!(bus.take_reads().is_empty());
    // Still in reset for three polls, done on the fourth; each poll is one
    // BMCR read.
    let polls: Vec<bool> = (0..5).map(|_| phy.reset_done(&mut bus)).collect();
    assert_eq!(polls, [false, false, false, true, true]);
    assert_eq!(bus.take_reads(), [(1, reg::BMCR); 5]);
    Ok(())
}

#[test]
fn autonegotiation_advertises_full_duplex_only() -> Result {
    let mut bus = FakeMdio::with_phy(1, RTL8211E);
    // Whatever the PHY came up advertising (here everything, half duplex
    // and pause included) is replaced, not added to.
    bus.set(1, reg::ANAR, 0x0DE1);
    bus.set(1, reg::GBCR, 0x0300);
    let phy = found(&mut bus)?;
    bus.take_reads();

    phy.start_autonegotiation(&mut bus);
    // 100 and 10 full (bits 8 and 6) with the 802.3 selector; 1000 full
    // (bit 9); then autonegotiation on (bit 12) and restarted (bit 9).
    assert_eq!(ADVERTISE, 0x0141);
    assert_eq!(ADVERTISE_1000, 0x0200);
    assert_eq!(
        bus.writes,
        [
            (1, reg::ANAR, 0x0141),
            (1, reg::GBCR, 0x0200),
            (1, reg::BMCR, 0x1200),
        ]
    );
    // Written whole: nothing read first.
    assert!(bus.take_reads().is_empty());
    Ok(())
}

/// PHYSR with the real-time link and resolved bits set, at `speed_bits`,
/// full duplex if `full`.
fn physr_up(speed_bits: u16, full: bool) -> u16 {
    let duplex = if full { physr::FULL_DUPLEX } else { 0 };
    (speed_bits << physr::SPEED_SHIFT) | duplex | physr::RESOLVED | physr::LINK
}

#[test]
fn physr_fields() {
    assert_eq!(physr::SPEED_MASK, 0xC000);
    assert_eq!(physr::FULL_DUPLEX, 0x2000);
    assert_eq!(physr::RESOLVED, 0x0800);
    assert_eq!(physr::LINK, 0x0400);
    assert_eq!(Speed::from_physr(0x0000), Some(Speed::Mbps10));
    assert_eq!(Speed::from_physr(0x4000), Some(Speed::Mbps100));
    assert_eq!(Speed::from_physr(0x8000), Some(Speed::Mbps1000));
    assert_eq!(Speed::from_physr(0xC000), None);
    // The other fields do not leak into the speed.
    assert_eq!(Speed::from_physr(0xBFFF), Some(Speed::Mbps1000));
    assert_eq!(Duplex::from_physr(0x2000), Duplex::Full);
    assert_eq!(Duplex::from_physr(0xDFFF), Duplex::Half);
}

#[test]
fn link_up_at_each_resolved_speed() -> Result {
    let cases = [
        (0b10, true, Speed::Mbps1000, Duplex::Full),
        (0b01, true, Speed::Mbps100, Duplex::Full),
        (0b00, true, Speed::Mbps10, Duplex::Full),
        (0b01, false, Speed::Mbps100, Duplex::Half),
    ];
    for (bits, full, speed, duplex) in cases {
        let mut bus = FakeMdio::with_phy(1, RTL8211E);
        let phy = found(&mut bus)?;
        bus.set(1, reg::PHYSR, physr_up(bits, full));
        bus.take_reads();
        let link = phy.link(&mut bus);
        assert_eq!(link, Link::Up { speed, duplex });
        assert_eq!(link.mode(), Some((speed, duplex)));
        // A usable PHYSR is one read; BMSR is not touched.
        assert_eq!(bus.take_reads(), [(1, reg::PHYSR)]);
    }
    Ok(())
}

#[test]
fn link_down() -> Result {
    let mut bus = FakeMdio::with_phy(1, RTL8211E);
    let phy = found(&mut bus)?;
    // Resolved fields left over from a link that has since dropped do not
    // count: the real-time link bit decides.
    bus.set(1, reg::PHYSR, physr_up(0b10, true) & !physr::LINK);
    bus.set(1, reg::BMSR, bmsr::AN_COMPLETE);
    let link = phy.link(&mut bus);
    assert_eq!(link, Link::Down);
    assert_eq!(link.mode(), None);
    Ok(())
}

#[test]
fn autonegotiation_done_but_speed_invalid_assumes_1000_full() -> Result {
    // The case behind lwipopts.h's CONFIG_LINKSPEED1000: autonegotiation
    // complete, the link up, and the speed unreadable: not resolved (with
    // or without stale speed bits), or resolved to the reserved value.
    let unresolved = physr::LINK | 0x8000 | physr::FULL_DUPLEX;
    let reserved = physr_up(0b11, true);
    // A BMSR as a linked PHY reads it: its abilities, autonegotiation
    // complete, link up.
    let bmsr_done = 0x796D;
    assert_ne!(bmsr_done & bmsr::AN_COMPLETE, 0);
    for value in [unresolved, physr::LINK, reserved] {
        let mut bus = FakeMdio::with_phy(1, RTL8211E);
        let phy = found(&mut bus)?;
        bus.set(1, reg::PHYSR, value);
        bus.set(1, reg::BMSR, bmsr_done);
        bus.take_reads();
        let link = phy.link(&mut bus);
        assert_eq!(link, Link::Assumed { physr: value });
        assert_eq!(link.mode(), Some((Speed::Mbps1000, Duplex::Full)));
        assert_eq!(bus.take_reads(), [(1, reg::PHYSR), (1, reg::BMSR)]);
    }
    assert_eq!(Link::ASSUMED, (Speed::Mbps1000, Duplex::Full));
    Ok(())
}

#[test]
fn link_without_speed_before_autonegotiation_completes_is_down() -> Result {
    // Nothing is assumed while the PHY is still negotiating: the link task
    // polls again and sees the resolved speed once it is there.
    let mut bus = FakeMdio::with_phy(1, RTL8211E);
    let phy = found(&mut bus)?;
    let bmsr_negotiating = 0x7949;
    assert_eq!(bmsr_negotiating & bmsr::AN_COMPLETE, 0);
    bus.set(1, reg::PHYSR, physr::LINK);
    bus.set(1, reg::BMSR, bmsr_negotiating);
    assert_eq!(phy.link(&mut bus), Link::Down);

    bus.set(1, reg::PHYSR, physr_up(0b10, true));
    assert_eq!(
        phy.link(&mut bus),
        Link::Up {
            speed: Speed::Mbps1000,
            duplex: Duplex::Full
        }
    );
    Ok(())
}

#[test]
fn an_assumed_link_is_the_same_mode_whatever_physr_reads() {
    let a = Link::Assumed { physr: 0x0400 };
    let b = Link::Assumed { physr: 0xE400 };
    assert_ne!(a, b);
    assert_eq!(a.mode(), b.mode());
    // And the same mode as a real 1000 full link, so a link task does not
    // see a change when the PHY starts resolving the speed.
    let up = Link::Up {
        speed: Speed::Mbps1000,
        duplex: Duplex::Full,
    };
    assert_eq!(a.mode(), up.mode());
}

#[test]
fn full_bring_up_sequence() -> Result {
    let mut bus = FakeMdio::with_phy(1, RTL8211E);
    bus.reset_reads = 2;
    let phy = found(&mut bus)?;
    phy.start_reset(&mut bus);
    let polls = (1..=10).find(|_| phy.reset_done(&mut bus));
    assert_eq!(polls, Some(3));
    phy.start_autonegotiation(&mut bus);
    assert_eq!(
        bus.writes,
        [
            (1, reg::BMCR, bmcr::RESET),
            (1, reg::ANAR, ADVERTISE),
            (1, reg::GBCR, ADVERTISE_1000),
            (1, reg::BMCR, bmcr::AN_ENABLE | bmcr::AN_RESTART),
        ]
    );
    Ok(())
}

const UP_1000_FULL: Link = Link::Up {
    speed: Speed::Mbps1000,
    duplex: Duplex::Full,
};

#[test]
fn link_watch_starts_down_and_is_quiet_while_down() {
    let mut watch = LinkWatch::new();
    assert_eq!(watch.update(Link::Down), None);
    assert_eq!(watch.update(Link::Down), None);
}

#[test]
fn link_watch_reports_up_once_then_down_once() {
    let mut watch = LinkWatch::new();
    assert_eq!(
        watch.update(UP_1000_FULL),
        Some(LinkChange::Up {
            speed: Speed::Mbps1000,
            duplex: Duplex::Full,
            assumed_physr: None,
        })
    );
    assert_eq!(watch.update(UP_1000_FULL), None);
    assert_eq!(watch.update(Link::Down), Some(LinkChange::Down));
    assert_eq!(watch.update(Link::Down), None);
}

#[test]
fn link_watch_reports_a_new_speed_without_going_down() {
    let mut watch = LinkWatch::new();
    watch.update(UP_1000_FULL);
    let up_100_half = Link::Up {
        speed: Speed::Mbps100,
        duplex: Duplex::Half,
    };
    assert_eq!(
        watch.update(up_100_half),
        Some(LinkChange::Up {
            speed: Speed::Mbps100,
            duplex: Duplex::Half,
            assumed_physr: None,
        })
    );
}

#[test]
fn link_watch_passes_on_the_physr_of_an_assumed_link() {
    let mut watch = LinkWatch::new();
    let physr = physr::LINK | physr::SPEED_MASK;
    assert_eq!(
        watch.update(Link::Assumed { physr }),
        Some(LinkChange::Up {
            speed: Speed::Mbps1000,
            duplex: Duplex::Full,
            assumed_physr: Some(physr),
        })
    );
    // Neither a different unresolved reading nor the PHY resolving the
    // same mode is a change: the MAC is already where it should be.
    assert_eq!(watch.update(Link::Assumed { physr: physr::LINK }), None);
    assert_eq!(watch.update(UP_1000_FULL), None);
}

#[test]
fn scan_reports_whatever_phy_answers_first() {
    let other = PhyId {
        id1: 0x001C,
        id2: 0xC916,
    };
    let mut bus = FakeMdio::with_phy(3, other);
    bus.place(7, RTL8211E);
    assert_eq!(phy::scan(&mut bus), Some((3, other)));
}

#[test]
fn scan_finds_nothing_on_an_empty_bus_or_one_held_low() {
    // An empty bus reads 0xFFFF everywhere.
    assert_eq!(phy::scan(&mut FakeMdio::default()), None);
    // A PHY whose identifier reads all zeros is a line held low.
    let mut held_low = FakeMdio::with_phy(1, PhyId { id1: 0, id2: 0 });
    held_low.place(0, PhyId { id1: 0, id2: 0 });
    assert_eq!(phy::scan(&mut held_low), None);
}
