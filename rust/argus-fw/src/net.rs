//! GEM0, the RTL8211 PHY and the IP stack: MAC 00:0a:35:00:01:02, static
//! 192.168.1.10/24 with gateway 192.168.1.1, as the C's `main.c` sets up
//! lwIP.
//!
//! [`start`] resets the PHY on MIO 9, brings up GEM0 with its descriptor
//! lists in the uncached section, finds the PHY, starts autonegotiation and
//! spawns two tasks: the embassy-net runner, which moves frames between the
//! GEM and smoltcp, and the link task, which polls the PHY every 100 ms and
//! follows each change with the GEM's speed, the stack's link state and an
//! `eth:` line. The C does neither in a task: lwIP's Xilinx port forces
//! 1000 Mb/s at boot (see [`argus_fw_core::phy`]) and never looks at the
//! link again.
//!
//! The pins are left as Vivado's ps7_init muxed them (MIO 16 to 27 RGMII,
//! 52 and 53 MDIO, LVCMOS 1.8 V), which is what the C runs on. The HAL's
//! `new_with_mio_eth_0` would reconfigure them for the Zedboard's HSTL, so
//! this uses the constructor that leaves the MIO alone.
//!
//! The frame buffers are in cached DDR, and the HAL cleans each one before
//! the GEM sends it and invalidates each one before smoltcp reads it (its
//! default `eth-cache-maintenance` feature). The descriptors the GEM and
//! the CPU both poll are in the uncached section instead, where no
//! maintenance can be forgotten.

use arbitrary_int::u5;
use argus_fw_core::lines::{
    ETH_LINK_DOWN, ETH_NO_PHY, EthLinkUp, EthPhy, EthPhyUnknown, EthSpeedAssumed,
};
use argus_fw_core::phy::{self, LinkChange, LinkWatch, Rtl8211};
use embassy_executor::Spawner;
use embassy_net::driver::LinkState;
use embassy_net::{Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_time::{Duration, Ticker, Timer};
use static_cell::ConstStaticCell;
use zynq7000_hal::clocks::Clocks;
use zynq7000_hal::eth::embassy_net::{DescriptorsAndBuffers, Driver, update_link_state};
use zynq7000_hal::eth::{
    AlignedBuffer, ClockConfig, ClockDivSet, Duplex, Ethernet, EthernetConfig, EthernetId,
    EthernetLowLevel, MTU, Speed, calculate_mdc_clk_div, mdio, rx_descr, tx_descr,
};
use zynq7000_hal::gpio::{Output, PinState, mio};
use zynq7000_hal::mmu::{section_attrs::SHAREABLE_DEVICE, update_l1_table};
use zynq7000_hal::{Interrupt, SpiInterrupt, cache, register_interrupt};

use crate::console::console;
use crate::read_global_timer;

/// The C's `mac_ethernet_address`: Xilinx's OUI, the address the lwIP
/// examples use.
pub const MAC_ADDRESS: [u8; 6] = [0x00, 0x0a, 0x35, 0x00, 0x01, 0x02];

/// The PS's address on the bench network.
pub const IP_ADDRESS: Ipv4Address = Ipv4Address::new(192, 168, 1, 10);

/// The bench network, 255.255.255.0.
const PREFIX_LEN: u8 = 24;

/// The gateway, unused on the bench but set as the C sets it.
const GATEWAY: Ipv4Address = Ipv4Address::new(192, 168, 1, 1);

/// Receive descriptors, each with its 1536-byte buffer. A replay burst is
/// up to 21 datagrams back to back (about 250 us at 1 Gb/s); 32 slots hold
/// a whole burst with room for the ARP and ping traffic around it, where
/// the Zedboard example's 16 would overrun.
const RX_SLOTS: usize = 32;

/// Transmit descriptors. The PS sends one telemetry frame per 50 ms bin,
/// one replay request per half and the odd ARP or ping reply; 16 frames
/// in flight is far more than that ever queues.
const TX_SLOTS: usize = 16;

/// Sockets the stack has room for: the telemetry and the replay UDP
/// sockets, which later tasks open.
const SOCKETS: usize = 2;

/// The 1 MB section memory.x calls UNCACHED, mapped as shareable device
/// memory so that every descriptor access reaches DDR.
const UNCACHED_SECTION: u32 = 0x0400_0000;

/// How long MIO 9 holds the PHY in reset: the RTL8211E and F need at
/// least 10 ms.
const PHY_RESET_LOW: Duration = Duration::from_millis(10);

/// How long after reset before the PHY answers on MDIO: at least 150 ms.
const PHY_RESET_SETTLE: Duration = Duration::from_millis(150);

/// How often the link task reads the PHY's status.
const LINK_POLL: Duration = Duration::from_millis(100);

// Placed in the uncached section, which XSDB does not load (NOLOAD):
// `take` writes every descriptor before the GEM is given the list.
#[unsafe(link_section = ".uncached")]
static RX_DESCRIPTORS: rx_descr::DescriptorList<RX_SLOTS> = rx_descr::DescriptorList::new();
#[unsafe(link_section = ".uncached")]
static TX_DESCRIPTORS: tx_descr::DescriptorList<TX_SLOTS> = tx_descr::DescriptorList::new();

static RX_BUFFERS: ConstStaticCell<[AlignedBuffer; RX_SLOTS]> =
    ConstStaticCell::new([AlignedBuffer([0; MTU]); RX_SLOTS]);
static TX_BUFFERS: ConstStaticCell<[AlignedBuffer; TX_SLOTS]> =
    ConstStaticCell::new([AlignedBuffer([0; MTU]); TX_SLOTS]);

static STACK_RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());

/// The PHY's registers through the GEM's PHY maintenance register. Each
/// access blocks for its MDIO frame, 64 MDC cycles at about 2.3 MHz, so
/// about 28 us.
struct GemMdio<'a>(&'a mut mdio::Mdio);

impl phy::Mdio for GemMdio<'_> {
    fn read(&mut self, phy: u8, reg: u8) -> u16 {
        // Masked to the field's five bits, since `u5::new` panics on more.
        self.0
            .read_blocking(u5::new(phy & 0x1F), u5::new(reg & 0x1F))
    }

    fn write(&mut self, phy: u8, reg: u8, value: u16) {
        self.0
            .write_blocking(u5::new(phy & 0x1F), u5::new(reg & 0x1F), value);
    }
}

/// Brings up the PHY, GEM0 and the stack, and spawns the runner and the
/// link task. Returns the stack the UDP sockets will be opened on, or
/// `None` when there is no network to run: no RTL8211E or RTL8211F answered (an
/// `eth:` line says what did, if anything), or one of the tasks could not
/// be spawned.
///
/// Takes about 160 ms, nearly all of it the PHY reset, which is slept
/// through so the other tasks run meanwhile.
pub async fn start(
    spawner: Spawner,
    gem: zynq7000_hal::pac::eth::MmioRegisters<'static>,
    phy_reset: mio::Pin<mio::Mio9>,
    clocks: &Clocks,
) -> Option<Stack<'static>> {
    // Only fails for an address that is not on a 1 MB boundary.
    update_l1_table(UNCACHED_SECTION, SHAREABLE_DEVICE).ok()?;

    // The PHY's reset is active low. Driving it here, before the GEM
    // exists, also means the PHY comes up with its power-on defaults
    // whatever an earlier image left in it.
    let mut phy_reset = Output::new_for_mio(phy_reset, PinState::Low);
    Timer::after(PHY_RESET_LOW).await;
    phy_reset.set_high();
    Timer::after(PHY_RESET_SETTLE).await;

    // The GEM's reference clock divisors for each speed, from the IO PLL
    // as the registers set it (125 MHz for 1000 Mb/s).
    let (clk_divs, _) = ClockDivSet::calculate_for_rgmii_and_io_clock(clocks.io_clocks());
    let mut eth = new_gem(gem, clocks, &clk_divs)?;
    let bufs = descriptors_and_buffers(&mut eth)?;
    register_interrupt(Interrupt::Spi(SpiInterrupt::Eth0), on_gem0_interrupt);
    eth.start();

    // After the hardware reset the PHY is in its power-on state, so no
    // soft reset: only the advertisement is set and negotiation restarted.
    let mut mdio = GemMdio(eth.mdio_mut());
    let Some(phy) = Rtl8211::probe(&mut mdio) else {
        match phy::scan(&mut mdio) {
            Some((addr, id)) => console!("{}", EthPhyUnknown { addr, id }),
            None => console!("{ETH_NO_PHY}"),
        }
        return None;
    };
    console!("{}", EthPhy { id: phy.id() });
    phy.start_autonegotiation(&mut mdio);

    let driver = Driver::new(&eth, MAC_ADDRESS, bufs);
    let config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(IP_ADDRESS, PREFIX_LEN),
        gateway: Some(GATEWAY),
        dns_servers: Default::default(),
    });
    // The seed only picks smoltcp's ephemeral ports; the timer's count at
    // this point is as good as any.
    let (stack, runner) = embassy_net::new(
        driver,
        config,
        STACK_RESOURCES.try_take()?,
        read_global_timer(),
    );

    spawner.spawn(stack_runner(runner).ok()?);
    spawner.spawn(link(eth, phy, clk_divs).ok()?);
    Some(stack)
}

/// GEM0, reset and configured for 1000 Mb/s full duplex (the link task
/// changes that if the PHY negotiates something else), its MDC below
/// 2.5 MHz, and the MAC address set. The receiver and transmitter stay off
/// until the descriptor lists are in place.
fn new_gem(
    gem: zynq7000_hal::pac::eth::MmioRegisters<'static>,
    clocks: &Clocks,
    clk_divs: &ClockDivSet,
) -> Option<Ethernet> {
    let ll = EthernetLowLevel::new(gem)?;
    let config = EthernetConfig::new(
        ClockConfig::new(clk_divs.cfg_1000_mbps),
        calculate_mdc_clk_div(clocks.arm_clocks())?,
        MAC_ADDRESS,
    );
    Some(Ethernet::new(ll, config))
}

/// Initialises both descriptor lists, points the GEM at them and bundles
/// them with their buffers for the driver. `None` only if called twice.
fn descriptors_and_buffers(eth: &mut Ethernet) -> Option<DescriptorsAndBuffers> {
    let rx_bufs = RX_BUFFERS.try_take()?;
    let tx_bufs = TX_BUFFERS.try_take()?;

    // SAFETY: `try_take` above returns `Some` once only, so this is the
    // only call of either `take`, which is the one rule they have: no
    // second reference to the lists ever exists.
    let (rx_list, tx_list) = unsafe { (RX_DESCRIPTORS.take(), TX_DESCRIPTORS.take()) };
    let mut rx_descr = rx_descr::DescriptorListWrapper::new(rx_list.as_mut_slice());
    let mut tx_descr = tx_descr::DescriptorListWrapper::new(tx_list.as_mut_slice());
    rx_descr.init_with_aligned_bufs(rx_bufs.as_slice());
    tx_descr.init_or_reset();

    // The buffers were zeroed through the cache at startup, so their lines
    // may be dirty. One written back after the GEM had put a frame there
    // would overwrite it with zeros. `init_with_aligned_bufs` cleans every
    // buffer but the last; this covers all of them. The array is 32-byte
    // aligned and a whole number of lines long, so this cannot fail.
    let rx_addr = u32::try_from(rx_bufs.as_ptr() as usize).ok()?;
    cache::clean_and_invalidate_data_cache_range(rx_addr, size_of_val(rx_bufs)).ok()?;

    eth.set_rx_buf_descriptor_base_address(rx_descr.base_addr());
    eth.set_tx_buf_descriptor_base_address(tx_descr.base_addr());
    DescriptorsAndBuffers::new(rx_descr, rx_bufs, tx_descr, tx_bufs)
}

/// GEM0's interrupt: wakes the runner on a frame sent or received. The
/// error flags it returns (receive overrun, AHB errors) are not reported
/// yet.
///
/// # Safety
///
/// None needed: `unsafe` only because the HAL's interrupt map holds
/// `unsafe fn()`. It is called from the IRQ handler only.
unsafe fn on_gem0_interrupt() {
    let _ = zynq7000_hal::eth::embassy_net::on_interrupt(EthernetId::Eth0);
}

/// The embassy-net runner: smoltcp's poll loop over the GEM driver.
#[embassy_executor::task]
async fn stack_runner(mut runner: Runner<'static, Driver>) -> ! {
    runner.run().await
}

/// Polls the PHY every 100 ms and, on a change, sets the GEM's speed and
/// duplex, tells the stack and prints the `eth:` line. Owns the GEM for
/// its MDIO interface and its clock settings; the driver holds only the
/// descriptor lists.
///
/// One poll is one PHYSR read, bracketed on the RTL8211F by two page
/// selects (and a BMSR read when the speed is unresolved). At about 28 us
/// per MDIO frame that is about 85 us on the F, inside the executor's
/// 100 us budget; the rare unresolved case adds one frame more.
#[embassy_executor::task]
async fn link(mut eth: Ethernet, phy: Rtl8211, clk_divs: ClockDivSet) -> ! {
    let mut watch = LinkWatch::new();
    let mut ticker = Ticker::every(LINK_POLL);
    loop {
        ticker.next().await;
        let state = phy.link(&mut GemMdio(eth.mdio_mut()));
        match watch.update(state) {
            None => {}
            Some(LinkChange::Down) => {
                update_link_state(LinkState::Down);
                console!("{ETH_LINK_DOWN}");
            }
            Some(LinkChange::Up {
                speed,
                duplex,
                assumed_physr,
            }) => {
                if let Some(physr) = assumed_physr {
                    console!("{}", EthSpeedAssumed { physr });
                }
                eth.configure_clock_and_speed_duplex(
                    gem_speed(speed),
                    gem_duplex(duplex),
                    &clk_divs,
                );
                update_link_state(LinkState::Up);
                console!("{}", EthLinkUp { speed, duplex });
            }
        }
    }
}

/// The HAL's name for a speed the PHY driver decoded.
const fn gem_speed(speed: phy::Speed) -> Speed {
    match speed {
        phy::Speed::Mbps10 => Speed::Mbps10,
        phy::Speed::Mbps100 => Speed::Mbps100,
        phy::Speed::Mbps1000 => Speed::Mbps1000,
    }
}

/// The HAL's name for a duplex the PHY driver decoded.
const fn gem_duplex(duplex: phy::Duplex) -> Duplex {
    match duplex {
        phy::Duplex::Half => Duplex::Half,
        phy::Duplex::Full => Duplex::Full,
    }
}
