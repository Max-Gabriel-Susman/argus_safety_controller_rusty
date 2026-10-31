//! Differential tests: the C firmware code and its Rust port, side by side.
//!
//! The C replay client (`argus_replay_client.h`) and the frame packing of
//! `argus_net.c` are compiled into a small C shim, and the tests in
//! `tests/` drive them and the Rust crates through the same seeded random
//! scenarios, checking after every step that the two agree. The C is the
//! specification, so any difference is a bug in the port (or a deliberate
//! change that the test has to be taught about, in a commit that says why).
//!
//! This crate holds what the tests share: the C side behind a safe wrapper
//! ([`c`]), a simulated relay ([`relay`]) and the random number generator
//! that makes each run repeatable from its seed ([`rng`]).
//!
//! It runs on the host only. The C is built for the machine running the
//! tests, so the crate is never built for the A9, and it is the one host
//! crate with an `unsafe` item: the `extern` block that declares the shim's
//! functions.
//!
//! Each run prints its seed when it fails. To repeat it:
//!
//! ```text
//! ARGUS_DIFFTEST_SEED=0x1234abcd cargo test -p argus-difftest
//! ```
//!
//! `ARGUS_DIFFTEST_SCENARIOS` sets how many scenarios a run tries (10,000
//! by default).

use std::fmt;

pub mod c;
pub mod relay;
pub mod rng;

/// Why a test failed. A test that returns `Err` is reported with the
/// error's `Debug` output; for a `String` that is the text escaped onto
/// one line. This one prints the message as written, line breaks and all,
/// so the two snapshots of a difference line up one above the other.
pub struct Failure(pub String);

impl fmt::Debug for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self(message)
    }
}
