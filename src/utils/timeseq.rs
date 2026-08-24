use std::hint::spin_loop;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct TimeSeq {
    pub timestamp: u64,
    pub sequence: u16,
}

impl TimeSeq {
    pub fn new() -> Self {
        GENERATOR.generate()
    }

    pub fn now() -> Self {
        GENERATOR.generate()
    }

    pub const fn from_parts(timestamp: u64, sequence: u16) -> Self {
        Self {
            timestamp,
            sequence,
        }
    }
}

impl Default for TimeSeq {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TimeSeqGenerator {
    // The upper 48 bits store the timestamp and the lower 16 bits store the sequence.
    state: AtomicU64,
}

impl TimeSeqGenerator {
    const SEQ_BITS: u32 = 16;
    const TIMESTAMP_BITS: u32 = 64 - Self::SEQ_BITS;
    const SEQ_MASK: u64 = (1 << Self::SEQ_BITS) - 1;
    const TIMESTAMP_MASK: u64 = (1 << Self::TIMESTAMP_BITS) - 1;

    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    pub fn generate(&self) -> TimeSeq {
        loop {
            let current_timestamp = Self::current_timestamp();
            assert!(
                current_timestamp <= Self::TIMESTAMP_MASK,
                "TimeSeq timestamp does not fit in 48 bits"
            );

            let current_state = self.state.load(Ordering::Relaxed);
            let last_timestamp = current_state >> Self::SEQ_BITS;
            let last_sequence = current_state & Self::SEQ_MASK;

            let (timestamp, sequence) = if current_timestamp > last_timestamp {
                (current_timestamp, 0)
            } else if last_sequence < Self::SEQ_MASK {
                // This also handles a clock moving backwards by preserving logical time.
                (last_timestamp, last_sequence + 1)
            } else {
                // Wait until the timestamp advances instead of wrapping and duplicating IDs.
                spin_loop();
                continue;
            };
            let new_state = (timestamp << Self::SEQ_BITS) | sequence;

            if self
                .state
                .compare_exchange_weak(
                    current_state,
                    new_state,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return TimeSeq::from_parts(timestamp, sequence as u16);
            }
        }
    }

    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_default()
    }
}

impl Default for TimeSeqGenerator {
    fn default() -> Self {
        Self::new()
    }
}

static GENERATOR: TimeSeqGenerator = TimeSeqGenerator::new();

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn issued_values_are_strictly_ordered() {
        let generator = TimeSeqGenerator::new();
        let first = generator.generate();
        let second = generator.generate();

        assert!(second > first);
        assert_ne!(first, second);
    }

    #[test]
    fn concurrent_issuance_does_not_overlap() {
        let generator = TimeSeqGenerator::new();
        let values = thread::scope(|scope| {
            (0..8)
                .map(|_| scope.spawn(|| (0..32).map(|_| generator.generate()).collect::<Vec<_>>()))
                .map(|thread| thread.join().expect("issuer thread panicked"))
                .flatten()
                .collect::<Vec<_>>()
        });
        let mut sorted = values.clone();
        sorted.sort_unstable();

        sorted.dedup();
        assert_eq!(sorted.len(), values.len());
    }
}
