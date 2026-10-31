//! A small seeded random number generator, so every run can be repeated
//! from the seed it prints.
//!
//! SplitMix64: one 64-bit add and a mixing function per number. Plenty for
//! choosing test scenarios, and a dozen lines instead of a dependency.

/// The environment variable that fixes the run's seed.
pub const SEED_VAR: &str = "ARGUS_DIFFTEST_SEED";

/// SplitMix64 state.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator that starts from `seed`.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 32 random bits.
    pub fn next_u32(&mut self) -> u32 {
        let [high @ .., _, _, _, _] = self.next_u64().to_be_bytes();
        u32::from_be_bytes(high)
    }

    /// A number in `0..n`, or 0 when `n` is 0. The modulo bias is far too
    /// small to matter here.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi.saturating_sub(lo).saturating_add(1))
    }

    /// [`range`](Self::range) for `u16` bounds.
    pub fn range_u16(&mut self, lo: u16, hi: u16) -> u16 {
        u16::try_from(self.range(lo.into(), hi.into())).unwrap_or(hi)
    }

    /// [`range`](Self::range) for `u32` bounds.
    pub fn range_u32(&mut self, lo: u32, hi: u32) -> u32 {
        u32::try_from(self.range(lo.into(), hi.into())).unwrap_or(hi)
    }

    /// True `num` times in `den`.
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }

    /// Mixes the order of `items`: Fisher-Yates.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = usize::try_from(self.below(i as u64 + 1)).unwrap_or(i);
            items.swap(i, j);
        }
    }
}

/// The seed for this run: [`SEED_VAR`] if it is set (decimal, or hex with
/// `0x`), otherwise one taken from the clock, so each run covers new
/// ground and prints what it needs to repeat it.
///
/// # Errors
///
/// A message naming the variable if it is set but not a number.
pub fn run_seed() -> Result<u64, String> {
    match std::env::var(SEED_VAR) {
        Ok(text) => {
            let text = text.trim();
            let parsed = match text.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => text.parse(),
            };
            parsed.map_err(|e| format!("{SEED_VAR}={text}: {e}"))
        }
        Err(_) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?;
            // The low bits change fastest; mixing once spreads them out.
            let nanos = u64::try_from(now.as_nanos() & u128::from(u64::MAX)).unwrap_or(0);
            Ok(Rng::new(nanos).next_u64())
        }
    }
}
