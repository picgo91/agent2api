//! 网关 API Key 的**导入合并**（导出文件里的 `apiKeys` 段）。
//!
//! ── 为什么导出要带上 Key ────────────────────────────────────
//! Key 列表住在 `config.json` 的 `apiKeys` 字段（`mod.rs` 的模块头），
//! 与账号文件是两处数据。只导账号不导 Key，换机器后客户端手里的那把 Key
//! 在新机器上不存在 → 网关免鉴权（没有启用的 Key 就不鉴权，见 `mod.rs`），
//! 或者用户重新建一把、客户端全要改。所以 v3 起导出文件多一段 `apiKeys`。
//!
//! ── 合并语义：命中即**跳过**，绝不覆盖 ────────────────────────
//! Key 的身份是**明文 Key 本身**（客户端发过来的就是它），不是 `id`
//! （`id` 只是管理接口 `PATCH /api/keys/{id}` 的句柄）。本机已有同一把
//! Key 时**跳过**，不拿文件里的值去改本机记录，理由有三条：
//!   1. **安全方向单调**：文件里的 `enabled` / `allowedProviders` /
//!      `allowedModels` 属于另一台机器、可能是几周前的状态。覆盖它可能把
//!      用户刚禁用的 Key 重新启用、或抹掉刚收紧的白名单 —— 一次「导入」
//!      静默放宽了鉴权边界，这个方向的错误不可逆。
//!   2. **重复导入要幂等**：同一份文件导两次，第二次全部跳过（而不是
//!      把时间戳之类的东西改一遍）。
//!   3. 与 `add` 的既有口径一致：重复的 Key 在 `add` 里就是「已存在」拒绝。
//! 想恢复某把 Key 的名称或白名单，在设置页改比靠导入顺手。
//!
//! ── 为什么读侧一律宽容（沿用 `mod.rs` 的硬不变量）────────────
//! 模块头那条「升级绝不能让已有 Key 失效」在这里同样成立，而且导入比升级
//! 更严重（丢一条 = 客户端立刻 401）。于是：
//!   - **不校验长度下限**：`add` 要求 ≥ `MIN_KEY_LENGTH`，导入不要求 ——
//!     1.x 遗留字段 `apiKey` 里的短 Key 本来就能用（`entries_from` 照收），
//!     拿导入当门禁会把用户手里能用的 Key 挡在门外。只拒空串。
//!   - **不校验 provider id 是否已注册**：`allowedProviders` 里出现本机
//!     没有的 provider id 照收（那家的定义可能稍后才导入，或用户正在删）。
//!     白名单里有不存在的家只是「路由不到」，不影响这把 Key 本身可用。
//!   - 白名单的容错归一复用 `string_list`（去空白、大小写去重）。
//!
//! ── id 的处理 ──────────────────────────────────────────────
//! 文件里的 id 只在**本机还没被占用**时沿用（句柄跨机器保持一致，便于
//! 回灌后继续用同一套自动化脚本按 id 操作）；被别的 Key 占用则换一个
//! 真正唯一的新 id，**绝不覆盖**。id 不参与匹配，所以换 id 不影响
//! 「同一把 Key 跳过」的判定。
//!
//! `createdAt` / `enabled` 随文件带过来（这是一次**还原**，不是合并）：
//! 文件里缺失或 ≤ 0 时才回落到本机当前时间，免得界面显示 1970 年。
//!
//! 单条坏数据只记一条警告并继续，不毁掉整批；**有新增才落盘**
//! （nothing-to-do 时不写 —— 免得把只读文件里的时间戳也一并改掉，
//! 也避免把 1.x 的 `apiKey` 旧字段无端固化成 `apiKeys`）。
//!
//! 本文件挂在 `api_keys` 模块下而不是 `account_transfer` 下：条目形状
//! （七个契约键 + 两个白名单的容错归一）是本模块的私有知识，放在模块树内
//! 就不必把 `ApiKeyEntry` / `string_list` / `save` 公开给兄弟模块。

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::{list, save, string_list, ApiKeyEntry};
use crate::server::logging;

/// 一条 Key 的处理结果（结构化警告：账号导入端直接并进 `errors` 数组）
pub(crate) struct KeyWarning {
    pub id: String,
    pub message: String,
}

/// 合并报告：新增 / 跳过多少把，以及逐条警告
pub(crate) struct MergeReport {
    pub added: usize,
    pub skipped: usize,
    pub warnings: Vec<KeyWarning>,
}

/// 把导入文件里的 API Key 段合并进本机配置（必要时落盘）。
///
/// `items` 是导出文件 `apiKeys` 键下的原始数组。整体失败（写盘不成功）
/// 返回 Err，调用方必须当成整批导入失败。
pub(crate) fn merge_imported(items: &[Value]) -> Result<MergeReport, String> {
    let mut current = list();
    let mut report = MergeReport { added: 0, skipped: 0, warnings: Vec::new() };
    // 两张去重表分开：existing 用于「本机已有」（命中即跳过），
    // file_seen 用于「文件内重复」（先到的赢，与自定义提供商定义同一取向）
    let existing: HashSet<String> = current.iter().map(|entry| entry.key.clone()).collect();
    let mut file_seen: HashSet<String> = HashSet::new();
    let mut taken_ids: HashSet<String> = current.iter().map(|entry| entry.id.clone()).collect();

    for item in items {
        let Some(object) = item.as_object() else {
            report.warnings.push(KeyWarning {
                id: String::new(),
                message: "API Key 记录必须是 JSON 对象，该条已跳过".to_string(),
            });
            continue;
        };
        let label = text(object, "id");
        let Some(entry) = entry_of(object) else {
            report.warnings.push(KeyWarning {
                id: label,
                message: "API Key 记录缺少 key，该条已跳过".to_string(),
            });
            continue;
        };
        let id = if entry.id.is_empty() { label } else { entry.id };

        if existing.contains(&entry.key) {
            report.skipped += 1;
            report.warnings.push(KeyWarning {
                id,
                message: "本机已存在相同的 API Key，该条已跳过（不覆盖本机的名称与限制）"
                    .to_string(),
            });
            continue;
        }
        if !file_seen.insert(entry.key.clone()) {
            report.skipped += 1;
            report.warnings.push(KeyWarning {
                id,
                message: "导出文件里重复出现的 API Key，仅采纳第一条".to_string(),
            });
            continue;
        }

        let mut entry = entry;
        if !taken_ids.contains(&entry.id) {
            taken_ids.insert(entry.id.clone());
        } else {
            entry.id = allocate_id(&taken_ids);
            taken_ids.insert(entry.id.clone());
        }
        if entry.created_at <= 0 {
            entry.created_at = logging::now_ms();
        }
        current.push(entry);
        report.added += 1;
    }

    // 有新增才写：全跳过时不动配置（本机就是最终状态）
    if report.added > 0 && !save(&current) {
        return Err(
            "保存失败：API Key 写入未成功（请检查磁盘空间与配置目录权限）".to_string(),
        );
    }
    Ok(report)
}

/// 导入用的一条 Key → 契约形状。
///
/// 与 `ApiKeyEntry::from_value` 的**唯一**区别：id 缺失不算坏数据
/// （可以现分配一个），`from_value` 则是解析已有配置、缺 id 就丢 —— 那条
/// 「不许因字段缺失丢记录」的不变量在**写入侧**靠"缺了当场补齐"来兑现，
/// 而不是靠放宽解析。其余（trim、trim 空即无、白名单容错、
/// `enabled` 缺省为真）逐条沿用，保证「导出的东西导回来是原样」。
fn entry_of(object: &Map<String, Value>) -> Option<ApiKeyEntry> {
    let key = object.get("key")?.as_str()?.trim().to_string();
    if key.is_empty() {
        return None;
    }
    Some(ApiKeyEntry {
        id: text(object, "id"),
        name: object.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        key,
        // 与 from_value 同口径：只有显式 false 才算禁用
        enabled: !matches!(object.get("enabled"), Some(Value::Bool(false))),
        created_at: object.get("createdAt").and_then(Value::as_i64).unwrap_or(0),
        allowed_providers: string_list(object.get("allowedProviders")),
        allowed_models: string_list(object.get("allowedModels")),
    })
}

fn text(object: &Map<String, Value>, key: &str) -> String {
    object.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// 分配一个本机未被占用的 id（`add` 的 `k{时间戳:x}{序号}` 口径）。
///
/// `taken` 是有限集合，因此下面的循环必然终止；序号只用来避开同毫秒内
/// 连续导入多把时的撞名。
fn allocate_id(taken: &HashSet<String>) -> String {
    let stamp = logging::now_ms();
    let mut n = 0u32;
    loop {
        let candidate = format!("k{stamp:x}{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}
