//! The Realtek RTL8211 Ethernet PHY on the Arty Z7-20, over an [`Mdio`]
//! bus: the RTL8211E the board's reference manual names, and the RTL8211F
//! this board actually carries (it identifies as 001c:c916).
//!
//! The two differ where it matters here, in the PHY specific status
//! register that says what autonegotiation settled on: the E has it at
//! register 0x11 ([`reg::PHYSR`]), the F at register 0x1a of page 0xa43
//! ([`reg::PHYSR_F`]), with other bit positions. Everything else this
//! driver touches is IEEE 802.3 clause 22 and the same on both.
//!
//! The C firmware leaves the PHY to the Xilinx lwIP port
//! (`xemacpsif_physpeed.c`), and on this board that code fails: its
//! `get_Realtek_phy_speed()` sees autonegotiation complete and then reads a
//! speed it does not recognise (most likely because it decodes register
//! 0x11 as an E's on an F), so `lwipopts.h` gives up on autodetection and
//! forces 1000 Mb/s with `CONFIG_LINKSPEED1000`. This driver
//! autonegotiates, reads the outcome from the chip's own status register
//! and keeps the C's answer only for the case the C hit: negotiation done
//! but the speed not readable, where it assumes 1000 Mb/s full duplex (the
//! board's link is RGMII gigabit; per `lwipopts.h`, 100 does not link and
//! 1000 does) and says so ([`Link::Assumed`]).
//!
//! The driver holds no bus. Each call borrows one, because in the firmware
//! the MDIO interface lives inside the GEM driver, which the link task also
//! needs, to set the MAC's speed. Nothing here waits: a soft reset is
//! started by one call and polled by another, so the firmware can sleep
//! between polls instead of spinning on the executor.
//!
//! Modelled on the Marvell 88E1518 driver of the zynq7000-rs Zedboard BSP;
//! the vendor registers are the RTL8211E and RTL8211F datasheets' (the F's
//! as u-boot's Realtek driver also uses them), and 0 to 4 and 9 are the
//! IEEE 802.3 clause 22 ones every PHY has.

/// A clause 22 MDIO bus: 32 PHY addresses of 32 16-bit registers each.
///
/// The firmware implements it over the GEM's PHY maintenance register; the
/// tests implement it over an array. Addresses and register numbers are
/// below 32; an implementation may mask them to 5 bits.
pub trait Mdio {
    /// Read register `reg` of the PHY at `phy`. An address nothing answers
    /// at reads 0xFFFF, the MDIO line's pull-up.
    fn read(&mut self, phy: u8, reg: u8) -> u16;
    /// Write `value` to register `reg` of the PHY at `phy`.
    fn write(&mut self, phy: u8, reg: u8, value: u16);
}

/// The registers this driver uses.
pub mod reg {
    /// Basic mode control (IEEE): see [`bmcr`](super::bmcr).
    pub const BMCR: u8 = 0x00;
    /// Basic mode status (IEEE): see [`bmsr`](super::bmsr).
    pub const BMSR: u8 = 0x01;
    /// PHY identifier 1 (IEEE): OUI bits 3 to 18.
    pub const PHYID1: u8 = 0x02;
    /// PHY identifier 2 (IEEE): OUI bits 19 to 24, model, revision.
    pub const PHYID2: u8 = 0x03;
    /// Autonegotiation advertisement (IEEE): see [`anar`](super::anar).
    pub const ANAR: u8 = 0x04;
    /// 1000BASE-T control (IEEE): see [`gbcr`](super::gbcr).
    pub const GBCR: u8 = 0x09;
    /// PHY specific status (RTL8211E): see [`physr`](super::physr).
    pub const PHYSR: u8 = 0x11;
    /// PHY specific status (RTL8211F, page [`PHYSR_F_PAGE`]): see
    /// [`physr_f`](super::physr_f).
    pub const PHYSR_F: u8 = 0x1A;
    /// Page select (RTL8211F): which page registers 0x10 to 0x1E show. 0
    /// is the default page; 0 to 15 are the IEEE registers on any page.
    pub const PAGE_SELECT: u8 = 0x1F;
    /// The RTL8211F page that holds [`PHYSR_F`].
    pub const PHYSR_F_PAGE: u16 = 0x0A43;
}

/// BMCR bits.
pub mod bmcr {
    /// Soft reset. Self-clearing: reads 1 until the reset is over.
    pub const RESET: u16 = 1 << 15;
    /// Autonegotiation on. With it on, the forced speed and duplex bits
    /// are ignored.
    pub const AN_ENABLE: u16 = 1 << 12;
    /// Restart autonegotiation. Self-clearing.
    pub const AN_RESTART: u16 = 1 << 9;
}

/// BMSR bits.
pub mod bmsr {
    /// Autonegotiation has completed.
    pub const AN_COMPLETE: u16 = 1 << 5;
}

/// ANAR bits.
pub mod anar {
    /// 100BASE-TX full duplex.
    pub const FULL_100: u16 = 1 << 8;
    /// 10BASE-T full duplex.
    pub const FULL_10: u16 = 1 << 6;
    /// The selector field's value for IEEE 802.3.
    pub const SELECTOR_802_3: u16 = 0x0001;
}

/// GBCR bits.
pub mod gbcr {
    /// 1000BASE-T full duplex.
    pub const FULL_1000: u16 = 1 << 9;
}

/// PHYSR fields. The speed and duplex fields mean nothing until
/// [`RESOLVED`](physr::RESOLVED) is set.
pub mod physr {
    /// The resolved speed, bits 15:14: see [`Speed`](super::Speed).
    pub const SPEED_SHIFT: u32 = 14;
    /// The speed field, in place.
    pub const SPEED_MASK: u16 = 0b11 << SPEED_SHIFT;
    /// The resolved duplex: 1 full, 0 half.
    pub const FULL_DUPLEX: u16 = 1 << 13;
    /// Speed and duplex have been resolved, by autonegotiation or forcing.
    pub const RESOLVED: u16 = 1 << 11;
    /// The link as it is now, not latched like BMSR's link bit.
    pub const LINK: u16 = 1 << 10;
}

/// RTL8211F PHYSR fields. The speed and duplex fields mean nothing while
/// [`LINK`](physr_f::LINK) is clear.
pub mod physr_f {
    /// The resolved speed, bits 5:4, coded as the E's: see
    /// [`Speed`](super::Speed).
    pub const SPEED_SHIFT: u32 = 4;
    /// The speed field, in place.
    pub const SPEED_MASK: u16 = 0b11 << SPEED_SHIFT;
    /// The resolved duplex: 1 full, 0 half.
    pub const FULL_DUPLEX: u16 = 1 << 3;
    /// The link as it is now, not latched like BMSR's link bit.
    pub const LINK: u16 = 1 << 2;
}

/// What this driver advertises in [`reg::ANAR`]: 100 and 10 Mb/s, full
/// duplex only, and no pause, since the GEM is not set up to honour pause
/// frames.
pub const ADVERTISE: u16 = anar::FULL_100 | anar::FULL_10 | anar::SELECTOR_802_3;

/// What this driver advertises in [`reg::GBCR`]: 1000 Mb/s full duplex
/// only.
pub const ADVERTISE_1000: u16 = gbcr::FULL_1000;

/// Realtek's OUI, as [`PhyId::oui`] assembles it.
pub const REALTEK_OUI: u32 = 0x00_0732;

/// The model number of Realtek's RTL8211 family, as [`PhyId::model`]
/// reads it. The family's members share it and differ in the revision.
pub const RTL8211_MODEL: u8 = 0x11;

/// The revision that makes an RTL8211 an RTL8211E (the B is 2, the C 3,
/// the DN 4, the F 6), as [`PhyId::revision`] reads it.
pub const RTL8211E_REVISION: u8 = 5;

/// The revision that makes an RTL8211 an RTL8211F.
pub const RTL8211F_REVISION: u8 = 6;

/// The RTL8211 chips this driver knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    /// RTL8211E: PHYSR at register 0x11.
    Rtl8211e,
    /// RTL8211F: PHYSR at page 0xa43, register 0x1a.
    Rtl8211f,
}

/// The two identifier registers of a PHY, as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhyId {
    /// [`reg::PHYID1`].
    pub id1: u16,
    /// [`reg::PHYID2`].
    pub id2: u16,
}

impl PhyId {
    /// The vendor's OUI. The registers hold its bits 3 to 24 (bits 1 and 2
    /// are always 0 and are dropped): PHYID1 bits 3 to 18, PHYID2's top six
    /// bits 19 to 24, in IEEE order, which as a number is ID1 above the top
    /// six bits of ID2.
    pub const fn oui(self) -> u32 {
        ((self.id1 as u32) << 6) | ((self.id2 as u32) >> 10)
    }

    /// The vendor's model number, PHYID2 bits 9:4.
    pub const fn model(self) -> u8 {
        ((self.id2 >> 4) & 0x3F) as u8
    }

    /// The silicon revision, PHYID2 bits 3:0.
    pub const fn revision(self) -> u8 {
        (self.id2 & 0x0F) as u8
    }

    /// An RTL8211E: Realtek's OUI, the RTL8211 model and the E's revision.
    /// The whole identifier is checked because the F, whose PHYSR is laid
    /// out differently, has the same model number.
    pub const fn is_rtl8211e(self) -> bool {
        matches!(self.chip(), Some(Chip::Rtl8211e))
    }

    /// Which RTL8211 this is, by the whole identifier; `None` for any
    /// other PHY, including the other RTL8211 revisions.
    pub const fn chip(self) -> Option<Chip> {
        if self.oui() != REALTEK_OUI || self.model() != RTL8211_MODEL {
            return None;
        }
        match self.revision() {
            RTL8211E_REVISION => Some(Chip::Rtl8211e),
            RTL8211F_REVISION => Some(Chip::Rtl8211f),
            _ => None,
        }
    }
}

/// A link speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    /// 10 Mb/s.
    Mbps10,
    /// 100 Mb/s.
    Mbps100,
    /// 1000 Mb/s.
    Mbps1000,
}

impl Speed {
    /// The RTL8211E's PHYSR speed field: see [`from_field`](Self::from_field).
    pub const fn from_physr(physr: u16) -> Option<Self> {
        Self::from_field((physr & physr::SPEED_MASK) >> physr::SPEED_SHIFT)
    }

    /// The two-bit speed field both chips use, shifted down: 0b00 10,
    /// 0b01 100, 0b10 1000. The fourth value is reserved and gives `None`.
    pub const fn from_field(field: u16) -> Option<Self> {
        match field {
            0b00 => Some(Self::Mbps10),
            0b01 => Some(Self::Mbps100),
            0b10 => Some(Self::Mbps1000),
            _ => None,
        }
    }

    /// The speed in Mb/s, as the `eth: link up` line prints it.
    pub const fn mbps(self) -> u32 {
        match self {
            Self::Mbps10 => 10,
            Self::Mbps100 => 100,
            Self::Mbps1000 => 1000,
        }
    }
}

/// A link duplex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplex {
    /// One direction at a time.
    Half,
    /// Both directions at once.
    Full,
}

impl Duplex {
    /// The PHYSR duplex bit.
    pub const fn from_physr(physr: u16) -> Self {
        if physr & physr::FULL_DUPLEX != 0 {
            Self::Full
        } else {
            Self::Half
        }
    }

    /// The duplex as the `eth: link up` line prints it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Half => "half",
            Self::Full => "full",
        }
    }
}

/// The link as the PHY reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// No link, or autonegotiation still running.
    Down,
    /// Up, at the speed and duplex the PHY resolved.
    Up {
        /// The resolved speed.
        speed: Speed,
        /// The resolved duplex.
        duplex: Duplex,
    },
    /// Up and autonegotiation complete, but PHYSR does not say at what:
    /// the speed is unresolved or reads the reserved value. This is what
    /// the C build hit; the link runs at [`Link::ASSUMED`], as the C's
    /// forced 1000 Mb/s did.
    Assumed {
        /// PHYSR as read, for the console line that says so.
        physr: u16,
    },
}

impl Link {
    /// The speed and duplex assumed when the PHY cannot say: 1000 Mb/s
    /// full duplex, the only mode this board links in.
    pub const ASSUMED: (Speed, Duplex) = (Speed::Mbps1000, Duplex::Full);

    /// The speed and duplex to run the MAC at, or `None` while the link is
    /// down. A link task compares these, not the [`Link`] itself, to see a
    /// change: an assumed link whose PHYSR reading moves has not changed.
    pub const fn mode(self) -> Option<(Speed, Duplex)> {
        match self {
            Self::Down => None,
            Self::Up { speed, duplex } => Some((speed, duplex)),
            Self::Assumed { .. } => Some(Self::ASSUMED),
        }
    }

    /// Decode an RTL8211E PHYSR reading. `an_complete` is BMSR's
    /// autonegotiation complete bit; it is asked for only when PHYSR shows
    /// a link without a usable speed, the one case it decides.
    pub fn from_physr(physr: u16, an_complete: impl FnOnce() -> bool) -> Self {
        if physr & physr::LINK == 0 {
            return Self::Down;
        }
        let speed = if physr & physr::RESOLVED != 0 {
            Speed::from_physr(physr)
        } else {
            None
        };
        Self::resolve(physr, speed, Duplex::from_physr(physr), an_complete)
    }

    /// Decode an RTL8211F PHYSR reading, as [`from_physr`](Self::from_physr)
    /// does the E's. The F has no resolved bit: with the link up the speed
    /// field holds the outcome, and only its reserved value is unusable.
    pub fn from_physr_f(physr: u16, an_complete: impl FnOnce() -> bool) -> Self {
        if physr & physr_f::LINK == 0 {
            return Self::Down;
        }
        let speed = Speed::from_field((physr & physr_f::SPEED_MASK) >> physr_f::SPEED_SHIFT);
        let duplex = if physr & physr_f::FULL_DUPLEX != 0 {
            Duplex::Full
        } else {
            Duplex::Half
        };
        Self::resolve(physr, speed, duplex, an_complete)
    }

    /// A link up at `speed` if the status register gave one; else assumed,
    /// once negotiation is complete.
    fn resolve(
        physr: u16,
        speed: Option<Speed>,
        duplex: Duplex,
        an_complete: impl FnOnce() -> bool,
    ) -> Self {
        match speed {
            Some(speed) => Self::Up { speed, duplex },
            // A link is reported while negotiation is still settling; wait
            // for the PHY to finish before assuming anything.
            None if an_complete() => Self::Assumed { physr },
            None => Self::Down,
        }
    }
}

/// A change of the link that the MAC and the IP stack must follow, as
/// [`LinkWatch::update`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkChange {
    /// The link came up, or came back at another speed or duplex: set the
    /// MAC to these and print the `eth: link up` line.
    Up {
        /// The speed to run the MAC at.
        speed: Speed,
        /// The duplex to run the MAC at.
        duplex: Duplex,
        /// PHYSR as read when the mode is [`Link::ASSUMED`] rather than
        /// resolved, for the `eth: speed unresolved` line printed first.
        assumed_physr: Option<u16>,
    },
    /// The link went down.
    Down,
}

/// The link mode last acted on, so that a link task polling the PHY acts
/// only on changes. It compares [`Link::mode`], so an assumed link whose
/// PHYSR reading moves is not a change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkWatch {
    mode: Option<(Speed, Duplex)>,
}

impl LinkWatch {
    /// Starts with the link down, as the MAC and the stack do.
    pub const fn new() -> Self {
        Self { mode: None }
    }

    /// Take one poll of the link; `Some` when it differs from the last.
    pub fn update(&mut self, link: Link) -> Option<LinkChange> {
        let mode = link.mode();
        if mode == self.mode {
            return None;
        }
        self.mode = mode;
        Some(match mode {
            None => LinkChange::Down,
            Some((speed, duplex)) => LinkChange::Up {
                speed,
                duplex,
                assumed_physr: match link {
                    Link::Assumed { physr } => Some(physr),
                    Link::Down | Link::Up { .. } => None,
                },
            },
        })
    }
}

/// The first PHY that answers on the bus, whatever it is, in
/// [`Rtl8211::probe`]'s order: its address and identifier. An address
/// reading 0xFFFF:0xFFFF (nothing driving the line) or 0x0000:0x0000 (the
/// line held low) is taken as no PHY. For saying what is there when the
/// probe finds no RTL8211E.
pub fn scan(mdio: &mut impl Mdio) -> Option<(u8, PhyId)> {
    (1..32).chain(0..1).find_map(|addr| {
        let id = PhyId {
            id1: mdio.read(addr, reg::PHYID1),
            id2: mdio.read(addr, reg::PHYID2),
        };
        let absent = (id.id1 == 0xFFFF && id.id2 == 0xFFFF) || (id.id1 == 0 && id.id2 == 0);
        (!absent).then_some((addr, id))
    })
}

/// An RTL8211E or RTL8211F found on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rtl8211 {
    addr: u8,
    id: PhyId,
    chip: Chip,
}

impl Rtl8211 {
    /// Find the PHY. Addresses 1 to 31 are tried first and 0 last, because
    /// an RTL8211 also answers at 0, the broadcast address, whatever its
    /// strapped address is (on the Arty, 1). `None` when no address reads
    /// an RTL8211E's or RTL8211F's identifier.
    pub fn probe(mdio: &mut impl Mdio) -> Option<Self> {
        (1..32).chain(0..1).find_map(|addr| {
            let id = PhyId {
                id1: mdio.read(addr, reg::PHYID1),
                id2: mdio.read(addr, reg::PHYID2),
            };
            id.chip().map(|chip| Self { addr, id, chip })
        })
    }

    /// Which of the two it is.
    pub const fn chip(&self) -> Chip {
        self.chip
    }

    /// The MDIO address it answered at.
    pub const fn addr(&self) -> u8 {
        self.addr
    }

    /// Its identifier registers, for the `eth: phy` line.
    pub const fn id(&self) -> PhyId {
        self.id
    }

    /// Start a soft reset. Poll [`reset_done`](Self::reset_done) before
    /// anything else; the PHY ignores writes until it is over.
    pub fn start_reset(&self, mdio: &mut impl Mdio) {
        mdio.write(self.addr, reg::BMCR, bmcr::RESET);
    }

    /// Whether the soft reset is over: BMCR's reset bit has cleared.
    pub fn reset_done(&self, mdio: &mut impl Mdio) -> bool {
        mdio.read(self.addr, reg::BMCR) & bmcr::RESET == 0
    }

    /// Advertise 1000, 100 and 10 Mb/s full duplex ([`ADVERTISE`],
    /// [`ADVERTISE_1000`]) and restart autonegotiation. The registers are
    /// written whole, not read and modified, so what is advertised does not
    /// depend on what the PHY came up with.
    pub fn start_autonegotiation(&self, mdio: &mut impl Mdio) {
        mdio.write(self.addr, reg::ANAR, ADVERTISE);
        mdio.write(self.addr, reg::GBCR, ADVERTISE_1000);
        mdio.write(self.addr, reg::BMCR, bmcr::AN_ENABLE | bmcr::AN_RESTART);
    }

    /// The link now: one PHYSR read, and a BMSR read when PHYSR shows a
    /// link without a usable speed (see [`Link::from_physr`]). On the F the
    /// read is bracketed by selecting [`reg::PHYSR_F_PAGE`] and going back
    /// to page 0, so that between polls the PHY is on its default page.
    pub fn link(&self, mdio: &mut impl Mdio) -> Link {
        let addr = self.addr;
        let an_complete = |mdio: &mut _| read_an_complete(mdio, addr);
        match self.chip {
            Chip::Rtl8211e => {
                let physr = mdio.read(addr, reg::PHYSR);
                Link::from_physr(physr, || an_complete(mdio))
            }
            Chip::Rtl8211f => {
                mdio.write(addr, reg::PAGE_SELECT, reg::PHYSR_F_PAGE);
                let physr = mdio.read(addr, reg::PHYSR_F);
                mdio.write(addr, reg::PAGE_SELECT, 0);
                Link::from_physr_f(physr, || an_complete(mdio))
            }
        }
    }
}

/// BMSR's autonegotiation complete bit, of the PHY at `addr`.
fn read_an_complete(mdio: &mut impl Mdio, addr: u8) -> bool {
    mdio.read(addr, reg::BMSR) & bmsr::AN_COMPLETE != 0
}
