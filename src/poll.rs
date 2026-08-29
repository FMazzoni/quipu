//! The client-side poll loop shared by `wait` and `watch`.
//!
#![doc = include_str!("../docs/modules/poll.md")]

use std::time::Duration;

/// Polls before the interval starts growing. At the default, ten seconds.
///
/// Not one: the interesting phase of a barrier is the one where tasks are
/// moving, and backing off after a single unchanged answer would slow exactly
/// the case a caller is watching.
const QUIET_POLLS: u32 = 20;

/// Default ceiling, as a multiple of the caller's interval.
const MAX_MULTIPLE: u32 = 10;

/// A poll interval that grows while nothing is happening and snaps back when
/// something does.
pub(crate) struct Interval {
    base: Duration,
    max: Duration,
    current: Duration,
    quiet: u32,
}

impl Interval {
    /// `max_ms` of `None` is [`MAX_MULTIPLE`] times the base.
    pub(crate) fn new(base_ms: u64, max_ms: Option<u64>) -> Self {
        // A zero interval is a spin, and over a socket it is a spin against
        // someone else's process. One millisecond is still absurd and is the
        // caller's business; zero is not representable as a poll at all.
        let base = Duration::from_millis(base_ms.max(1));
        let max = match max_ms {
            Some(ms) => Duration::from_millis(ms.max(1)),
            None => base * MAX_MULTIPLE,
        };
        Interval {
            base,
            max: max.max(base),
            current: base,
            quiet: 0,
        }
    }

    /// The answer changed: poll at full rate again.
    pub(crate) fn saw_change(&mut self) {
        self.quiet = 0;
        self.current = self.base;
    }

    /// The answer did not change: earn one step of backoff.
    pub(crate) fn saw_nothing(&mut self) {
        self.quiet = self.quiet.saturating_add(1);
        if self.quiet > QUIET_POLLS {
            self.current = (self.current * 2).min(self.max);
        }
    }

    /// Waits out the current interval.
    pub(crate) fn wait(&self) {
        std::thread::sleep(self.current);
    }

    #[cfg(test)]
    pub(crate) fn current_ms(&self) -> u128 {
        self.current.as_millis()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quiet earns backoff, up to the ceiling; a change gives it all back.
    #[test]
    fn the_interval_grows_only_while_nothing_happens() {
        let mut i = Interval::new(500, None);
        for _ in 0..QUIET_POLLS {
            i.saw_nothing();
            assert_eq!(i.current_ms(), 500, "no backoff inside the quiet window");
        }
        i.saw_nothing();
        assert_eq!(i.current_ms(), 1000);
        for _ in 0..20 {
            i.saw_nothing();
        }
        assert_eq!(i.current_ms(), 5000, "capped at ten times the base");
        i.saw_change();
        assert_eq!(i.current_ms(), 500, "a change snaps the interval back");
    }

    /// A caller that needs a tight release can turn the backoff off.
    #[test]
    fn an_explicit_ceiling_bounds_the_backoff() {
        let mut i = Interval::new(500, Some(500));
        for _ in 0..100 {
            i.saw_nothing();
        }
        assert_eq!(i.current_ms(), 500);
    }

    /// A zero interval is a spin against someone else's process.
    #[test]
    fn a_zero_interval_is_not_representable() {
        assert_eq!(Interval::new(0, Some(0)).current_ms(), 1);
    }
}
