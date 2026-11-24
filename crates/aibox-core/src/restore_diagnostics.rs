//! Session-local restore diagnostics. Only fixed outcome codes and counts are
//! exported, and only as part of the user's explicit diagnostic ZIP request.
//! No per-event history, timestamps, disk sidecar, or network reporting.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

pub const PAUSED_CODE: &str = "RESTORE_AUTOMATIC_PAUSED";

#[derive(Default)]
struct Counters {
    paused: AtomicU64,
}

impl Counters {
    fn record_paused(&self) {
        let _ = self
            .paused
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_add(1))
            });
    }

    fn snapshot(&self) -> Value {
        json!({
            "schema": "aibox-restore-diagnostics/v1",
            "scope": "current-session",
            "counts": { (PAUSED_CODE): self.paused.load(Ordering::Relaxed) },
        })
    }
}

static COUNTERS: Counters = Counters {
    paused: AtomicU64::new(0),
};

/// No caller-supplied values enter the diagnostic counters.
pub fn record_paused() {
    COUNTERS.record_paused();
}

pub(crate) fn snapshot() -> Value {
    COUNTERS.snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_the_fixed_code_and_saturates() {
        let counters = Counters::default();
        counters.record_paused();
        counters.record_paused();
        assert_eq!(
            counters.snapshot(),
            json!({
                "schema": "aibox-restore-diagnostics/v1",
                "scope": "current-session",
                "counts": {"RESTORE_AUTOMATIC_PAUSED": 2},
            })
        );
        counters.paused.store(u64::MAX, Ordering::Relaxed);
        counters.record_paused();
        assert_eq!(counters.snapshot()["counts"][PAUSED_CODE], u64::MAX);
    }
}
