//! The client for the Argus replay protocol, ported from
//! `argus_replay_client.h`.
//!
//! The PS asks the relay for a run of samples; the relay answers with a
//! burst of chunks, each a header and up to 1444 bytes of samples. The
//! client checks every chunk, places it by its index in the burst, and when
//! the deadline passes with chunks still missing, asks again for the first
//! gap. A fetch ends [`Complete`](State::Complete) or, once the retries are
//! spent, [`Failed`](State::Failed).
//!
//! One implementation, every transport. The protocol logic is here; the
//! socket, the clock and the destination are the caller's, through the
//! [`Transport`] and [`Sink`] traits and a millisecond count passed in. So
//! the same client fills a DDR buffer, a BRAM half or a test vector.
//!
//! ```
//! use argus_replay::{Client, State, Transport};
//!
//! struct Recorder(Vec<Vec<u8>>);
//! impl Transport for Recorder {
//!     type Error = ();
//!     fn send(&mut self, datagram: &[u8]) -> Result<(), ()> {
//!         self.0.push(datagram.to_vec());
//!         Ok(())
//!     }
//! }
//!
//! let mut client = Client::new(96, 200, 5).unwrap();
//! let mut net = Recorder(Vec::new());
//! client.fetch(&mut net, 0, 147, 0).unwrap();
//! assert_eq!(client.state(), State::Waiting);
//! assert_eq!(net.0.len(), 1);
//! // ... hand each received datagram to client.on_packet(), and call
//! // client.poll() as time passes, until client.is_done().
//! ```
//!
//! `no_std`, no allocation, and no unsafe code: the bytes-to-struct work is
//! zerocopy's.
#![no_std]
#![forbid(unsafe_code)]

mod client;
mod io;

pub use client::{Client, ConfigError, Counters, FetchError, Packet, Reject, State};
pub use io::{Sink, Transport};

/// The most chunks one burst may have.
///
/// A burst is bounded by the samples in a half divided by the smallest chunk
/// that fits an MTU. At 124 channels that is 5 samples per chunk, so 30
/// chunks. 32 gives headroom and lets the arrival set be one `u32`.
pub const MAX_CHUNKS: u16 = 32;
