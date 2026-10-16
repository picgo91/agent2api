//! Antigravity 项目发现（`loadCodeAssist` → 必要时 `onboardUser`）。
//!
//! 事实来源：spec §A.6。
//!
//! ── 为什么 project 是硬前置 ─────────────────────────────────
//! 推理信封的顶层 `project` 必须是一个真实项目（否则上游 4xx）；它来自
//! `loadCodeAssist` 的 `cloudaicompanionProject`。新账号第一次登录时该项目
//! 可能还没创建 —— 此时走 `onboardUser` 按 tier 开通，轮询最多 5 次（间隔
//! 2 秒，每次请求自带 15 秒超时）。
//!
//! ── 兜底 ────────────────────────────────────────────────────
//! 两条路都没拿到时，信封层用 [`protocol::DEFAULT_PROJECT`] 兜底（CLIProxyAPI
//! 的 `DEFAULT_PROJECT_ID`）—— 那是个公开的默认项目名，能让请求形态完整。
//! 本模块的 Err 只在**查询动作本身失败**时出现，供添加账号路径给出人话。

use serde_json::Value;

use crate::server::core::proxies::ResolvedProxy;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::auth;

/// 轮询参数（spec §A.6：≤5 次、间隔 2 秒）
const ONBOARD_ATTEMPTS: u32 = 5;
const ONBOARD_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// 缺省档位（tier 解析全都失败时的最后兜底，spec §A.6）
const DEFAULT_TIER: &str = "free-tier";

/// 从一个响应里按优先级提取 project：`cloudaicompanionProject` → `projectId`
/// → `project`；外层 `response` / `data` 包裹都看（spec §A.6）。
pub fn extract_project(payload: &Value) -> String {
    let mut roots: Vec<&Value> = vec![payload];
    for key in ["response", "data"] {
        if let Some(inner) = payload.get(key).filter(|item| item.is_object()) {
            roots.push(inner);
        }
    }
    for root in roots {
        for key in ["cloudaicompanionProject", "projectId", "project"] {
            match root.get(key) {
                Some(Value::String(text)) if !text.trim().is_empty() => {
                    return text.trim().to_string()
                }
                // 对象形态（`{"id": "…"}` / `{"project": "…"}`）也认
                Some(Value::Object(object)) => {
                    for nested in ["id", "project", "projectId", "name"] {
                        if let Some(text) = object.get(nested).and_then(Value::as_str) {
                            let text = text.trim();
                            if !text.is_empty() {
                                return text.to_string();
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    String::new()
}

/// tier 解析：`allowedTiers[].isDefault` → 它的 `id`；否则 `currentTier.id`；
/// 否则 `"free-tier"`（spec §A.6）。
pub fn resolve_tier(payload: &Value) -> String {
    let root = payload
        .get("response")
        .filter(|item| item.is_object())
        .unwrap_or(payload);
    if let Some(tiers) = root.get("allowedTiers").and_then(Value::as_array) {
        if let Some(default) = tiers
            .iter()
            .find(|tier| tier.get("isDefault").and_then(Value::as_bool) == Some(true))
        {
            if let Some(id) = default
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return id.to_string();
            }
        }
    }
    root.get("currentTier")
        .and_then(|tier| tier.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_TIER.to_string())
}

/// 确保拿到一个可用 project：`loadCodeAssist` → 没有则 `onboardUser` 轮询。
///
/// 返回值可能为空串（两条路都走到但上游没给项目名）—— 调用方按兜底处理；
/// **Err 表示查询/开通动作本身失败**（401/403/网络），照原样上报给人看。
pub async fn ensure_project(
    token: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<String, GatewayError> {
    let load = auth::load_code_assist(token, proxy).await?;
    let project = extract_project(&load);
    if !project.is_empty() {
        return Ok(project);
    }
    let tier = resolve_tier(&load);
    logging::verbose(
        "[Antigravity]",
        &format!("账号尚无项目，按档位 {tier} 开通（最多 {ONBOARD_ATTEMPTS} 次轮询）"),
    );
    for attempt in 1..=ONBOARD_ATTEMPTS {
        let response = auth::onboard_user(token, &tier, proxy).await?;
        let done = response
            .get("done")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if done {
            let project = extract_project(&response);
            if !project.is_empty() {
                return Ok(project);
            }
        }
        if attempt < ONBOARD_ATTEMPTS {
            tokio::time::sleep(ONBOARD_INTERVAL).await;
        }
    }
    Err(GatewayError::with_status(
        504,
        "Antigravity 项目创建未完成（等待超时），请稍后重试或重新登录",
    ))
}
