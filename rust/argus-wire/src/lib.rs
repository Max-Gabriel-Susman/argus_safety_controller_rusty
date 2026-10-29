//! The Argus wire contract, in Rust.
//!
//! What every datagram between the board and the host has to agree on. For
//! now that is the checksum; the packet layouts from `argus_wire.h` and the
//! checks that validate a packet before anything reads it come next.
//!
//! The crate is `no_std`: it uses `core` only, with no allocator and no
//! operating system underneath, so the same code runs in host tests and on
//! the Cortex-A9.
//!
//! It also forbids `unsafe` code. The one job here that needs it, turning
//! bytes into structs, belongs to zerocopy, whose derives check at compile
//! time that each struct is safe to convert.
#![no_std]
#![forbid(unsafe_code)]
pub mod crc;
