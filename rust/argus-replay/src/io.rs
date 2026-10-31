//! The two things the client needs from outside: a way to send a request,
//! and a place to put the samples that come back.
//!
//! The C client takes a send callback plus a context pointer, and a `dst`
//! pointer it writes through with memcpy. Here both are traits, and neither
//! is stored in the client: each call that needs one is handed it. The
//! client is plain state, so it borrows nothing between calls and the
//! firmware can keep it in a task without lifetimes tying it to a socket or
//! to the BRAM.

/// Where requests go: a UDP socket on the board, a vector in a test.
pub trait Transport {
    /// What a failed send reports. The client passes it back to the caller
    /// untouched.
    type Error;

    /// Sends one datagram to the relay.
    ///
    /// The client does not retry on a send failure: that is a local fault,
    /// distinct from a lost datagram, and the caller sees it as the `Err`
    /// from [`fetch`](crate::Client::fetch) or [`poll`](crate::Client::poll).
    ///
    /// # Errors
    ///
    /// Whatever the transport could not do.
    fn send(&mut self, datagram: &[u8]) -> Result<(), Self::Error>;
}

/// Where the samples of a fetch land: a DDR buffer, a BRAM half, a test
/// vector.
///
/// Addressed in bytes from the start of the fetch. The samples are the
/// relay's little-endian `u16`s, copied as they arrived, as the C memcpy
/// does; on the little-endian A9 that is the `u16` array the PL reads.
pub trait Sink {
    /// How many bytes the destination holds. A chunk that would reach past
    /// this is rejected rather than written.
    fn capacity(&self) -> usize;

    /// Copies `payload` to `offset` bytes from the start.
    ///
    /// The client only calls this with `offset + payload.len()` at most
    /// [`capacity`](Sink::capacity), so an implementation need not check
    /// again.
    fn write(&mut self, offset: usize, payload: &[u8]);
}

/// A byte slice is a sink: the plain case, for a buffer in RAM.
impl Sink for [u8] {
    fn capacity(&self) -> usize {
        self.len()
    }

    fn write(&mut self, offset: usize, payload: &[u8]) {
        let end = offset.saturating_add(payload.len());
        // `get_mut` instead of indexing: out of range gives None, not a
        // panic. The client never asks for that, so nothing is lost here.
        if let Some(dst) = self.get_mut(offset..end) {
            dst.copy_from_slice(payload);
        }
    }
}
