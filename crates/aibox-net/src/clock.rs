//! 时钟抽象（TTL、缓存、复活语义全部可注入时钟离线单测）。
//!
//! 语义：单调递增的毫秒计数（不是墙钟）。进程内各处共享同一 [`Clock`] 实例即可
//! 保持时间一致性；测试用 [`ManualClock`] 手动推进。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// 可注入时钟：返回单调毫秒计数。
pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now_ms(&self) -> u64;
}

/// 共享时钟句柄。
pub type SharedClock = Arc<dyn Clock>;

/// 进程真实时钟（自构造起单调）。
#[derive(Debug, Clone)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
}

/// 手动时钟（测试/诊断用）：`advance_ms` / `set_ms` 直接推进。
/// 公开导出是为了让依赖本 crate 的 aibox-netdoctor / aibox-core 测试复用。
#[derive(Debug, Default)]
pub struct ManualClock {
    ms: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance_ms(&self, ms: u64) {
        self.ms.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn set_ms(&self, ms: u64) {
        self.ms.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.ms.load(Ordering::SeqCst)
    }
}

/// 进程级真实时钟共享句柄。
pub fn system_clock() -> SharedClock {
    Arc::new(SystemClock::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_advances() {
        let c = ManualClock::new();
        assert_eq!(c.now_ms(), 0);
        c.advance_ms(500);
        c.advance_ms(1000);
        assert_eq!(c.now_ms(), 1500);
        c.set_ms(42);
        assert_eq!(c.now_ms(), 42);
    }

    #[test]
    fn system_clock_monotonic_nonzero_after_sleep() {
        let c = SystemClock::new();
        std::thread::sleep(std::time::Duration::from_millis(15));
        assert!(c.now_ms() >= 10, "elapsed should be >= 10ms: {}", c.now_ms());
    }
}
