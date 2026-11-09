//! 壳侧进程内状态：下载 job 注册表 + update_check 5min 缓存（全局单例，无 Tauri State 依赖）。

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Instant;

use aibox_core::update::UpdateInfo;

/// 下载 job：取消标志（进度闭包里轮询；置位 → 中断下载，保留 .part 断点）。
#[derive(Clone)]
pub struct JobHandle {
    pub cancel: Arc<AtomicBool>,
}

static JOBS: LazyLock<Mutex<HashMap<String, JobHandle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 锁（毒化时取内层继续，避免一处 panic 拖死全部 job）。
pub fn jobs() -> MutexGuard<'static, HashMap<String, JobHandle>> {
    JOBS.lock().unwrap_or_else(|p| p.into_inner())
}

/// update_check 5min 缓存。
static UPDATE_CACHE: LazyLock<Mutex<Option<(Instant, UpdateInfo)>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn update_cache() -> MutexGuard<'static, Option<(Instant, UpdateInfo)>> {
    UPDATE_CACHE.lock().unwrap_or_else(|p| p.into_inner())
}
