//! What the Argus firmware decides, as opposed to what it touches.
//!
//! The C firmware mixes the two: `acq_read_features()` reads a register and
//! does the power division on the next line. Here the arithmetic and the
//! checks live apart from the register reads, so they run in host tests
//! with sample values instead of only on the board.
//!
//! - [`acq`]: the register map of `argus_acq.h`, the feature words turned
//!   into counts and powers, the frame word identity check and the fabric
//!   revisions the firmware accepts.
//! - [`console`]: the ring behind the non-blocking console, from
//!   `argus_console.c`.
//! - [`lines`]: the exact text of every console line the C firmware prints.
//! - [`phy`]: the board's RTL8211E Ethernet PHY over an MDIO trait: find
//!   it, reset it, autonegotiate and read back the link.
//! - [`telemetry`]: the v3 frame each feature bin becomes, and the count
//!   of frames that never reached the network.
//! - [`timing`]: the per-report sums, maxima and averages in the stream
//!   lines.
//!
//! `no_std`, no allocation and no unsafe code.
#![no_std]
#![forbid(unsafe_code)]

pub mod acq;
pub mod console;
pub mod lines;
pub mod phy;
pub mod telemetry;
pub mod timing;
