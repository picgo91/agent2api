//! Antigravity 模型目录：34 个静态条目（spec 附录 1，逐键来自
//! `resources/official_models.json`）。
//!
//! ── 为什么是静态表（`supports_model_refresh()` 为 false）────────
//! 官方客户端把目录**打包在资源里**（不在网上拉）；上游确实有
//! `fetchAvailableModels` 接口，但它按账号返回可用子集且形态未在 spec 中
//! 固化 —— 拿它当刷新源会让「没有账号就没有清单」。CLIProxyAPI 同样内联
//! 静态表（`MaxCompletionTokens` 也来自这张表），本实现与之同取舍。
//!
//! ── 条目的键名口径（与另外几家对齐）──────────────────────────
//! `id` / `name` / `maxInputTokens` / `supportsImages` / `supportsReasoning` /
//! `supportsToolCall` / `isDefault` / `kind`（聚合层认识的一套），外加两个自有键：
//!   - `upstreamKey`：发给上游的 `model` 字段值（Antigravity 就是目录键本身，
//!     没有代号/通道前缀那一套，但保留这个键让路由层与其它家同形）；
//!   - `reasoningEfforts`：该模型自己声明的思考档位（构造请求时按它收敛，
//!     见 `protocol::resolve_effort`）。
//!
//! ── 上下文/输出上限是展示值 ─────────────────────────────────
//! `maxInputTokens` 只用于 `/v1/models` 展示（网关不按它截断）；`maxOutputTokens`
//! 参与 Claude 的封顶（spec B.2：非 Claude 一律删掉该字段，Claude 封顶
//! `MaxCompletionTokens` = 64000）。未经 spec 逐条核实的条目给保守值。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：零 unwrap/expect/panic。

use serde_json::{json, Value};

/// Claude 系思考档位（目录逐条同值）
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "max"];
/// Gemini 系思考档位
const GEMINI_EFFORTS: &[&str] = &["minimal", "low", "medium", "high"];
/// 图像生成模型没有思考
const NO_EFFORTS: &[&str] = &[];

/// gemini-3.1-flash-image 的原生请求类型是 `image_gen`（spec B.2）
pub const IMAGE_MODEL: &str = "gemini-3.1-flash-image";

/// (目录键, 展示名, maxInputTokens, 支持图片输入, 思考档位, maxOutputTokens)
#[rustfmt::skip]
const MODEL_SPECS: &[(&str, &str, u64, bool, &[&str], Option<u64>)] = &[
    ("claude-opus-4-6-thinking", "Claude Opus 4.6 (Thinking)", 250_000, true, CLAUDE_EFFORTS, Some(64_000)),
    ("claude-opus-5-5-high",     "Claude Opus 5.5 (High)",     250_000, true, CLAUDE_EFFORTS, Some(64_000)),
    ("claude-opus-5-5-low",      "Claude Opus 5.5 (Low)",      250_000, true, CLAUDE_EFFORTS, Some(64_000)),
    ("claude-opus-5-5-medium",   "Claude Opus 5.5 (Medium)",   250_000, true, CLAUDE_EFFORTS, Some(64_000)),
    ("claude-sonnet-4-6",        "Claude Sonnet 4.6",          250_000, true, CLAUDE_EFFORTS, Some(64_000)),
    ("claude-sonnet-5-5-high",   "Claude Sonnet 5.5 (High)",   250_000, true, CLAUDE_EFFORTS, Some(64_000)),
    ("claude-sonnet-5-5-low",    "Claude Sonnet 5.5 (Low)",    250_000, true, CLAUDE_EFFORTS, Some(64_000)),
    ("claude-sonnet-5-5-medium", "Claude Sonnet 5.5 (Medium)", 250_000, true, CLAUDE_EFFORTS, Some(64_000)),
    ("gemini-2.5-flash",         "Gemini 2.5 Flash",          1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-2.5-flash-lite",    "Gemini 2.5 Flash Lite",     1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-2.5-flash-thinking","Gemini 2.5 Flash (Thinking)",1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-2.5-pro",           "Gemini 2.5 Pro",            1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.1-flash-image",   "Gemini 3.1 Flash (Image)",     32_768, true, NO_EFFORTS,     None),
    ("gemini-3.1-flash-lite",    "Gemini 3.1 Flash Lite",     1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.1-pro-high",      "Gemini 3.1 Pro (High)",     1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.1-pro-low",       "Gemini 3.1 Pro (Low)",      1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.5-flash-extra-low","Gemini 3.5 Flash (Extra Low)",1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.5-flash-low",     "Gemini 3.5 Flash (Low)",    1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.6-flash-high",    "Gemini 3.6 Flash (High)",   1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.6-flash-low",     "Gemini 3.6 Flash (Low)",    1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.6-flash-medium",  "Gemini 3.6 Flash (Medium)", 1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.6-flash-tiered",  "Gemini 3.6 Flash (Tiered)", 1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.7-flash-high",    "Gemini 3.7 Flash (High)",   1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.7-flash-low",     "Gemini 3.7 Flash (Low)",    1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.7-flash-medium",  "Gemini 3.7 Flash (Medium)", 1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.7-flash-tiered",  "Gemini 3.7 Flash (Tiered)", 1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.8-flash-high",    "Gemini 3.8 Flash (High)",   1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.8-flash-low",     "Gemini 3.8 Flash (Low)",    1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.8-flash-medium",  "Gemini 3.8 Flash (Medium)", 1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3.8-flash-tiered",  "Gemini 3.8 Flash (Tiered)", 1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3-flash",           "Gemini 3 Flash",            1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-3-flash-agent",     "Gemini 3 Flash (Agent)",    1_048_576, true, GEMINI_EFFORTS, None),
    ("gemini-pro-agent",         "Gemini Pro (Agent)",        1_048_576, true, GEMINI_EFFORTS, None),
    ("gpt-oss-120b-medium",      "GPT-OSS 120B (Medium)",       131_072, false, GEMINI_EFFORTS, None),
];

/// 目录条目（聚合层形态 + 两个自有键）
pub fn list() -> Vec<Value> {
    MODEL_SPECS
        .iter()
        .map(|(key, name, window, images, efforts, max_output)| {
            let efforts: Vec<Value> = efforts
                .iter()
                .map(|level| Value::String((*level).to_string()))
                .collect();
            let mut item = json!({
                "id": key,
                "name": name,
                "maxInputTokens": window,
                "supportsImages": images,
                "supportsReasoning": !efforts.is_empty(),
                "supportsToolCall": true,
                // 客户端内置目录里 Gemini 3 Flash 是一般场景的默认项
                "isDefault": *key == "gemini-3-flash",
                "kind": "chat",
                "upstreamKey": key,
                "reasoningEfforts": efforts,
            });
            if let Some(max_output) = max_output {
                if let Some(object) = item.as_object_mut() {
                    object.insert("maxOutputTokens".to_string(), json!(max_output));
                }
            }
            item
        })
        .collect()
}

/// 客户端点名的模型 → 目录条目（`id` / `name` / `upstreamKey` 三个键、
/// trim + 忽略大小写；与聚合目录同一口径，避免「目录放行、转发层认不出」）。
pub fn resolve(model_name: &str) -> Option<Value> {
    let wanted = model_name.trim();
    if wanted.is_empty() {
        return None;
    }
    list().into_iter().find(|item| {
        ["id", "name", "upstreamKey"].iter().any(|key| {
            item.get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case(wanted))
        })
    })
}

/// 目录键 → 是否 Claude 系（决定 toolConfig 的 VALIDATED 与 maxOutputTokens 保留）
pub fn is_claude(key: &str) -> bool {
    key.to_ascii_lowercase().starts_with("claude-")
}

/// 目录键 → 是否图像生成模型（决定 `requestType` 与 requestId/sessionId 的取舍）
pub fn is_image(key: &str) -> bool {
    key.eq_ignore_ascii_case(IMAGE_MODEL)
}

/// 公开的重查入口（路由失败时的文案用）
pub fn known_names() -> Vec<String> {
    MODEL_SPECS
        .iter()
        .map(|(key, ..)| (*key).to_string())
        .collect()
}
