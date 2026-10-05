//! 每把网关 Key 的请求频率限制（进程内滑动窗口）。
//!
//! ── 目标 ──────────────────────────────────────────────────────
//! 一个客户端脚本拿一把 Key 猛打时，不该把上游额度耗光、把同一台机器上
//! 其它客户端饿死。按 Key 设一个「每分钟请求数」（RPM）上限就能挡住这种。
//!
//! ── 为什么是「进程内」而不是持久化 ────────────────────────────
//! 限流是**运行时状态**，不是配置：重启后从零开始完全合理（重启本来就打断了
//! 正在发生的洪峰），持久化它只会引入「重启后仍被历史计数拖着」的反直觉行为。
//! 与 `refresh_flight` 这类进程内去重同一取向。
//!
//! ── 为什么是固定窗口而不是滑动窗口 / 令牌桶 ──────────────────
//! 固定 60 秒窗口的实现是常数级、无额外内存、无需后台清理：
//! `{window_start, count}` 两个字段就够。它的已知缺点（窗口边界处可能瞬时
//! 放行接近 2×RPM）对「保护上游别被一个客户端打爆」这个目标完全可以接受；
//! 令牌桶要维护令牌与上次补充时刻、还要为每个 Key 定期补令牌，复杂度换来的
//! 平滑度在本项目不是需求。
//!
//! ── 内存增长边界 ──────────────────────────────────────────────
//! 表按 **Key 的 id** 建条目，条目数 ≤ Key 数量（管理面就几把到几十把），
//! 不会因为请求量增长。被删掉的 Key 留下的条目最多一个窗口大小那么旧，
//! 下次被同名 id 命中时自然重置，不构成泄漏。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 统计窗口长度：60 秒（RPM 就是「每分钟」）。
const WINDOW_MS: i64 = 60_000;

#[derive(Default)]
struct Counter {
    /// 当前窗口的起点（毫秒时间戳）
    window_start: i64,
    /// 当前窗口内已放行的请求数
    count: u32,
}

fn registry() -> &'static Mutex<HashMap<String, Counter>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Counter>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 毒锁恢复（与项目其它全局表同一取向：并发 panic 不该永久锁死限流）。
fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Counter>> {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 一次准入结果。
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// 放行
    Allow,
    /// 超限：附带建议的 `Retry-After` 秒数（当前窗口剩余时间，至少 1）
    Reject { retry_after_secs: u64 },
}

/// 判断某把 Key 的这次请求是否放行，并按需自增计数。
///
/// `rpm == 0` 表示**不限制**（默认；与白名单「空 = 不限制」同一取向），
/// 直接放行且不建条目。
///
/// `id` 用 Key 记录的 **id**（不是明文 Key）：明文不该在内存表里当键，
/// 且 id 在升级 / 改名时稳定。
pub fn check(id: &str, rpm: u32) -> Decision {
    if rpm == 0 {
        return Decision::Allow;
    }
    let now = now_ms();
    let mut table = lock();
    let counter = table.entry(id.to_string()).or_default();
    // 跨窗口：重置起点与计数
    if now - counter.window_start >= WINDOW_MS {
        counter.window_start = now;
        counter.count = 0;
    }
    if counter.count >= rpm {
        let elapsed = now - counter.window_start;
        let remaining = (WINDOW_MS - elapsed).max(0) as u64;
        // 至少给 1 秒，避免客户端在毫秒边界疯狂重试
        return Decision::Reject {
            retry_after_secs: (remaining / 1000).max(1),
        };
    }
    counter.count += 1;
    Decision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_rpm_is_unlimited() {
        assert_eq!(check("k-unlimited", 0), Decision::Allow);
        assert_eq!(check("k-unlimited", 0), Decision::Allow);
    }

    #[test]
    fn allows_up_to_rpm_then_rejects() {
        let id = "k-limit-test";
        for _ in 0..3 {
            assert_eq!(check(id, 3), Decision::Allow);
        }
        match check(id, 3) {
            Decision::Reject { retry_after_secs } => assert!(retry_after_secs >= 1),
            Decision::Allow => panic!("第 4 次应当被拒"),
        }
    }

    #[test]
    fn separate_ids_have_separate_buckets() {
        assert_eq!(check("k-a", 1), Decision::Allow);
        assert_eq!(check("k-b", 1), Decision::Allow);
        assert!(matches!(check("k-a", 1), Decision::Reject { .. }));
    }
}
