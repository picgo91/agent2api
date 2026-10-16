//! Antigravity 协议转换：OpenAI Chat Completions → `v1internal` 官方信封。
//!
//! 事实来源：`cpa-deploy/notes/antigravity-protocol-spec.md` §B/C.3（从 CLIProxyAPI
//! 与 Antigravity-Manager 两份参考实现逐行核对）。
//!
//! ── 两边长什么样 ────────────────────────────────────────────
//! 下游（本项目内部统一形态，见 `core::protocol` 的模块头）：
//! ```jsonc
//! { "model": "…", "messages": [{role, content, tool_calls, tool_call_id}],
//!   "tools": [{"type":"function","function":{name, description, parameters}}],
//!   "tool_choice": "auto", "temperature": 0.7, "max_tokens": 1024, "stream": true }
//! ```
//! 上游（`POST {base}/v1internal:streamGenerateContent?alt=sse`）：
//! ```jsonc
//! { "project": "…", "requestId": "agent-<uuid>",
//!   "request": { "contents": [{"role":"user","parts":[{"text":"hi"}]}],
//!                "systemInstruction": {"role":"user","parts":[{"text":"…"}]},
//!                "tools": [{"functionDeclarations":[{name, description, parameters}]}],
//!                "toolConfig": {"functionCallingConfig":{"mode":"VALIDATED"}},
//!                "generationConfig": {"temperature":0.7,"thinkingConfig":{…}},
//!                "sessionId": "-123…" },
//!   "model": "gemini-3-flash", "userAgent": "antigravity", "requestType": "agent" }
//! ```
//!
//! ── 几个必须照抄的点（都是上游/参考实现的实际约束）────────────
//!   1. **终帧判定**是「`finishReason` 与 `usageMetadata` 同帧」（spec C.4）——
//!      只有 usage 的帧不是结束，见 `stream.rs`；
//!   2. **非 Claude 一律删 `maxOutputTokens`**，Claude 封顶 64000（spec B.2）；
//!   3. **历史里的 functionCall part 要补 sentinel `thoughtSignature`**
//!      （Gemini 3+ 对缺签名的 functionCall 报错，spec B.3）；
//!   4. **`toolChoice` 显式给过才发 `toolConfig`**；Claude 例外 —— 有工具时
//!      恒 `VALIDATED`（spec B.2 的 CLIProxyAPI 行为）；
//!   5. **`system` 只在「对话开始前」进 `systemInstruction`**；中途出现的
//!      system/developer 降级为 user 文本、`<system-reminder>` 包裹（spec C.3）。
//!
//! ── hints 分两类（与另外几家同口径）───────────────────────────
//! 上游把额度问题放在 429（`RESOURCE_EXHAUSTED`），把凭证问题放在 401/403
//! （`UNAUTHENTICATED` / `PERMISSION_DENIED`），但错误帧里只有文案，因此
//! 状态码与文案两条都看（见 [`classify_upstream_error`]）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：零 unwrap/expect/panic。

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// 信封缺 project 时的兜底（CLIProxyAPI 的 `DEFAULT_PROJECT_ID`；Manager 用
/// `aicode-consumers` 占位，这里取 CLIProxyAPI —— 它是本实现的主要对照）
pub const DEFAULT_PROJECT: &str = "bamboo-precept-lgxtn";

/// Claude 系 `maxOutputTokens` 封顶（spec B.2 / 附录 1）
pub const CLAUDE_MAX_OUTPUT_TOKENS: u64 = 64_000;

/// 历史 functionCall 缺失签名时的哨兵（spec B.3；上游认识这个特殊值）
pub const SENTINEL_SIGNATURE: &str = "skip_thought_signature_validator";

/// 上游 `finishReason` → OpenAI 的四值域（`stream.rs` 在收尾处还会用
/// 「有没有工具调用」修正一次：上游带工具调用时也给 STOP）
pub fn finish_reason(raw: &str) -> &'static str {
    match raw.to_ascii_uppercase().as_str() {
        "MAX_TOKENS" | "MAX_OUTPUT_TOKENS" => "length",
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY" => {
            "content_filter"
        }
        "TOOL_CALLS" | "TOOL_USE" | "FUNCTION_CALL" => "tool_calls",
        _ => "stop",
    }
}

/// 额度类信号（429 之外还从文案里认）
const QUOTA_HINTS: &[&str] = &[
    "resource_exhausted",
    "resource exhausted",
    "quota",
    "rate limit",
    "rate_limit",
    "too many requests",
    "exceeded",
    "insufficient",
    "余额",
    "额度",
    "配额",
    "限流",
];

/// 鉴权类信号
const AUTH_HINTS: &[&str] = &[
    "unauthenticated",
    "unauthorized",
    "permission_denied",
    "invalid_grant",
    "invalid credentials",
    "invalid token",
    "invalid_token",
    "token expired",
    "token has been expired",
    "login required",
];

/// 上游错误的粗分类（转发层据此决定换账号 / 刷新 / 透传）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UpstreamKind {
    /// 额度用尽 / 限流 → 标账号冷却 + 换号
    Quota,
    /// 凭证失效 → 刷新后同账号重试一次
    Auth,
    /// 其余（内容拦截由 `content_block` 再判）
    Other,
}

/// 按 HTTP 状态码 + 文案分类。
pub fn classify_upstream_error(status: u16, message: &str) -> UpstreamKind {
    let lower = message.to_ascii_lowercase();
    if status == 401 || status == 403 {
        return UpstreamKind::Auth;
    }
    if status == 429 {
        return UpstreamKind::Quota;
    }
    if AUTH_HINTS.iter().any(|hint| lower.contains(hint)) {
        return UpstreamKind::Auth;
    }
    if QUOTA_HINTS.iter().any(|hint| lower.contains(hint)) {
        return UpstreamKind::Quota;
    }
    UpstreamKind::Other
}

/// 思考档位 → 模型自己声明的集合里最接近的一档（口径同 Accio：发一个
/// 没声明的档位要么被忽略要么 400；`off` 在外层就被拦下）。
pub fn resolve_effort(level: &str, efforts: &[String]) -> Option<String> {
    if efforts.is_empty() {
        return None;
    }
    let wanted = crate::server::core::model_rules::reasoning_rank(level)?;
    let mut best: Option<(usize, &String)> = None;
    for effort in efforts {
        let Some(rank) = crate::server::core::model_rules::reasoning_rank(effort) else {
            continue;
        };
        let distance = rank.abs_diff(wanted);
        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, effort));
        }
    }
    best.map(|(_, effort)| effort.clone())
        .or_else(|| efforts.first().cloned())
}

/// 从请求体里读客户端显式指定的思考档位（绑定让位判据用）
pub fn client_effort(body: &Value) -> Option<String> {
    crate::server::core::model_rules::read_client_level(body)
}

/// 一次请求转换的产出
pub struct OutboundBody {
    /// 已归一的上游请求体
    pub body: Value,
    /// 要不要下发思考内容（上游声明支持思考且档位不是「关」）
    pub thinking: bool,
}

/// 一次请求转换的中间状态
struct Converter {
    contents: Vec<Value>,
    system_texts: Vec<String>,
    /// 客户端 tool_call id → 清洗后的工具名（functionResponse 要靠它对上号）
    tool_names: HashMap<String, String>,
    /// 声明扫描产出的（原名字 → 清洗名）对照：历史里的 functionCall 必须
    /// 用**同一个**清洗结果，否则声明与历史对不上号
    name_map: Vec<(String, String)>,
    /// 已占用的工具名（去重后缀用）
    used_names: Vec<String>,
}

/// 工具名清洗：非法字符 → `_`、首字符补 `_`、截断 64、重名加 `_N`。
///
/// 与 CLIProxyAPI 的 sanitizer 同口径（spec B.2/C.3）。不做还原映射：
/// 工具名绝大多数本来就是合法标识符，清洗几乎恒等（见 `mod.rs` 的取舍说明）。
fn sanitize_name(raw: &str, used: &mut Vec<String>) -> String {
    let mut name: String = raw
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric()
                || character == '_'
                || character == '.'
                || character == '-'
            {
                character
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        name = "tool".to_string();
    }
    if !name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
    {
        name.insert(0, '_');
    }
    name.truncate(64);
    if used.iter().any(|existing| existing == &name) {
        let mut counter = 2_u32;
        loop {
            let suffix = format!("_{counter}");
            let base: String = name.chars().take(64 - suffix.len()).collect();
            let candidate = format!("{base}{suffix}");
            if !used.iter().any(|existing| existing == &candidate) {
                name = candidate;
                break;
            }
            counter += 1;
        }
    }
    used.push(name.clone());
    name
}

/// JSON Schema 清洗：删 spec 列出的键、`type` 值递归大写（spec B.2/B.3）。
fn clean_schema(value: &Value) -> Value {
    const DROP_KEYS: &[&str] = &[
        "$id",
        "$comment",
        "patternProperties",
        "prefill",
        "enumTitles",
        "enumDescriptions",
        "deprecated",
    ];
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, item) in map {
                if DROP_KEYS.contains(&key.as_str()) {
                    continue;
                }
                let cleaned = if key == "type" {
                    uppercase_type(item)
                } else {
                    clean_schema(item)
                };
                out.insert(key.clone(), cleaned);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(clean_schema).collect()),
        other => other.clone(),
    }
}

fn uppercase_type(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(text.to_ascii_uppercase()),
        Value::Array(items) => Value::Array(items.iter().map(uppercase_type).collect()),
        other => other.clone(),
    }
}

/// OpenAI 的 `arguments`（字符串或对象）→ functionCall 的 `args` 对象。
///
/// 解析成功且是对象 → 直接用；成功但不是对象 → `{"params": …}`；
/// 解析失败 → `{"params": 原串}`（spec C.3 的三种形态）。
fn tool_args_value(raw: &Value) -> Value {
    if raw.is_object() {
        return raw.clone();
    }
    let Some(text) = raw.as_str() else {
        return json!({});
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(map)) => Value::Object(map),
        Ok(other) => json!({ "params": other }),
        Err(_) => json!({ "params": text }),
    }
}

/// 一个 OpenAI content（字符串或分块数组）→ parts（文本 + 图片）。
fn content_to_parts(content: &Value, parts: &mut Vec<Value>) {
    match content {
        Value::String(text) => {
            if !text.is_empty() {
                parts.push(json!({ "text": text }));
            }
        }
        Value::Array(items) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") | Some("input_text") | Some("output_text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                parts.push(json!({ "text": text }));
                            }
                        }
                    }
                    Some("image_url") | Some("input_image") => {
                        let url = item
                            .get("image_url")
                            .and_then(|value| {
                                value
                                    .get("url")
                                    .and_then(Value::as_str)
                                    .or_else(|| value.as_str())
                            })
                            .or_else(|| item.get("url").and_then(Value::as_str))
                            .unwrap_or("");
                        if let Some(part) = image_part(url) {
                            parts.push(part);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// 图片：**只处理 base64 data URI**（spec C.3：远程 URL 丢弃 —— 上游取不到，
/// 静默丢比发一个必然失败的外链好）。
fn image_part(url: &str) -> Option<Value> {
    let url = url.trim();
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.contains("base64") || data.is_empty() {
        return None;
    }
    let mime = meta.split(';').next().unwrap_or("image/png");
    // 空 mime（`data:;base64,…`）按 PNG 兜底
    let mime = if mime.is_empty() { "image/png" } else { mime };
    Some(json!({ "inlineData": { "mimeType": mime, "data": data } }))
}

/// 相邻同角色合并（Gemini 对角色交替没有硬要求，合并能少一轮无意义往返）
fn push_parts(role: &str, parts: Vec<Value>, contents: &mut Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut() {
        let same_role = last.get("role").and_then(Value::as_str) == Some(role);
        if same_role {
            if let Some(existing) = last.get_mut("parts").and_then(Value::as_array_mut) {
                existing.extend(parts);
                return;
            }
        }
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

/// OpenAI 消息体 → contents + systemInstruction + 工具表。
fn convert_messages(body: &Value, converter: &mut Converter) {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for message in &messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        match role {
            "system" | "developer" => {
                let mut parts = Vec::new();
                content_to_parts(message.get("content").unwrap_or(&Value::Null), &mut parts);
                let text = parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    continue;
                }
                if converter.contents.is_empty() {
                    // 对话开始前的 system → systemInstruction（spec C.3）
                    converter.system_texts.push(text);
                } else {
                    // 中途的 system → 降级为 user 文本、<system-reminder> 包裹
                    push_parts(
                        "user",
                        vec![
                            json!({ "text": format!("<system-reminder>\n{text}\n</system-reminder>") }),
                        ],
                        &mut converter.contents,
                    );
                }
            }
            "assistant" => {
                let mut parts: Vec<Value> = Vec::new();
                if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
                    if !reasoning.is_empty() {
                        parts.push(json!({ "text": reasoning, "thought": true }));
                    }
                }
                content_to_parts(message.get("content").unwrap_or(&Value::Null), &mut parts);
                if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for (index, call) in tool_calls.iter().enumerate() {
                        let function = call.get("function").cloned().unwrap_or(Value::Null);
                        let raw_name = function
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim();
                        if raw_name.is_empty() {
                            continue;
                        }
                        // 声明扫描时已清洗过的名字走对照表（历史与声明必须一致）；
                        // 没声明过的（客户端裁掉了旧工具）才现洗一次
                        let declared = converter
                            .name_map
                            .iter()
                            .find(|(raw, _)| raw == raw_name)
                            .map(|(_, sanitized)| sanitized.clone());
                        let name = match declared {
                            Some(sanitized) => sanitized,
                            None => {
                                if converter.used_names.iter().any(|used| used == raw_name) {
                                    raw_name.to_string()
                                } else {
                                    sanitize_name(raw_name, &mut converter.used_names)
                                }
                            }
                        };
                        let client_id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("tool_{index}"));
                        converter.tool_names.insert(client_id.clone(), name.clone());
                        let args = function
                            .get("arguments")
                            .map(tool_args_value)
                            .unwrap_or_else(|| json!({}));
                        // 历史 functionCall 要带 sentinel 签名（spec B.3）
                        parts.push(json!({
                            "functionCall": { "id": client_id, "name": name, "args": args },
                            "thoughtSignature": SENTINEL_SIGNATURE,
                        }));
                    }
                }
                push_parts("model", parts, &mut converter.contents);
            }
            "tool" | "function" => {
                let client_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let name = message
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .or_else(|| converter.tool_names.get(&client_id).cloned())
                    .unwrap_or_else(|| "tool".to_string());
                let content = match message.get("content") {
                    Some(Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                // 结果保留为**字符串**（spec C.3：response.result），空 → "{}"
                let result = if content.is_empty() {
                    "{}"
                } else {
                    content.as_str()
                };
                let mut function_response = json!({
                    "name": name,
                    "response": { "result": result },
                });
                if !client_id.is_empty() {
                    function_response["id"] = Value::String(client_id.clone());
                }
                push_parts(
                    "user",
                    vec![json!({ "functionResponse": function_response })],
                    &mut converter.contents,
                );
            }
            _ => {
                let mut parts = Vec::new();
                content_to_parts(message.get("content").unwrap_or(&Value::Null), &mut parts);
                push_parts("user", parts, &mut converter.contents);
            }
        }
    }
    // 只有 system（没有任何对话轮）时：保证 contents 非空，否则上游会拒
    if converter.contents.is_empty() {
        if let Some(first) = converter.system_texts.first().cloned() {
            push_parts(
                "user",
                vec![json!({ "text": first })],
                &mut converter.contents,
            );
            converter.system_texts.clear();
        }
    }
}

/// 工具声明（单节点 `functionDeclarations` 数组，spec B.3）。
///
/// 返回声明节点与（原名字 → 清洗名）对照。声明按出现顺序清洗，历史里的
/// tool_calls 用同一张 `used_names` 表（`convert_messages` 已按同一顺序登记）。
fn build_tool_declarations(
    body: &Value,
    used: &mut Vec<String>,
) -> (Vec<Value>, Vec<(String, String)>) {
    let mut declarations = Vec::new();
    let mut name_map = Vec::new();
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return (declarations, name_map);
    };
    for tool in tools {
        let function = tool.get("function").unwrap_or(tool);
        let raw_name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if raw_name.is_empty() {
            continue;
        }
        let sanitized = sanitize_name(raw_name, used);
        let description = function
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        // `parametersJsonSchema` 是 Gemini 风格的别名（spec B.2），两个都认
        let parameters = function
            .get("parameters")
            .or_else(|| function.get("parametersJsonSchema"))
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        let mut declaration = Map::new();
        declaration.insert("name".to_string(), Value::String(sanitized.clone()));
        if !description.is_empty() {
            declaration.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        declaration.insert("parameters".to_string(), clean_schema(&parameters));
        declarations.push(Value::Object(declaration));
        name_map.push((raw_name.to_string(), sanitized));
    }
    (declarations, name_map)
}

/// `tool_choice` → `toolConfig.functionCallingConfig`（spec C.3；仅在客户端
/// 显式提供时映射 —— Claude 有工具时例外，恒 VALIDATED）。
fn build_tool_config(
    body: &Value,
    is_claude: bool,
    has_declarations: bool,
    name_map: &[(String, String)],
) -> Option<Value> {
    let choice = body.get("tool_choice");
    let mapped = match choice {
        Some(Value::String(text)) => match text.trim().to_ascii_lowercase().as_str() {
            "none" => Some(json!({ "mode": "NONE" })),
            "auto" => Some(json!({ "mode": "AUTO" })),
            "required" | "any" => Some(json!({ "mode": "ANY" })),
            _ => None,
        },
        Some(Value::Object(object)) => {
            let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "function" || kind == "allowed_tools" {
                let wanted = object
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let sanitized = name_map
                    .iter()
                    .find(|(raw, _)| raw == wanted)
                    .map(|(_, sanitized)| sanitized.clone());
                match sanitized {
                    Some(name) => Some(json!({
                        "mode": "ANY",
                        "allowedFunctionNames": [name],
                    })),
                    None => Some(json!({ "mode": "ANY" })),
                }
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(config) = mapped {
        return Some(json!({ "functionCallingConfig": config }));
    }
    // Claude：有工具时上游要求 VALIDATED（spec B.2）
    if is_claude && has_declarations {
        return Some(json!({ "functionCallingConfig": { "mode": "VALIDATED" } }));
    }
    None
}

/// `response_format` → `responseMimeType`（+ `responseSchema`，spec C.3）
fn apply_response_format(body: &Value, generation: &mut Map<String, Value>) {
    let Some(response_format) = body.get("response_format") else {
        return;
    };
    match response_format.get("type").and_then(Value::as_str) {
        Some("json_object") => {
            generation.insert(
                "responseMimeType".to_string(),
                Value::String("application/json".to_string()),
            );
        }
        Some("json_schema") => {
            generation.insert(
                "responseMimeType".to_string(),
                Value::String("application/json".to_string()),
            );
            if let Some(schema) = response_format
                .get("json_schema")
                .and_then(|value| value.get("schema"))
            {
                generation.insert("responseSchema".to_string(), clean_schema(schema));
            }
        }
        _ => {}
    }
}

/// `sessionId`：首条 user 文本的 sha256 前 8 字节（大端、清掉顶位）→ `-<u64>`
/// （spec B.3 的 CLIProxyAPI 派生；没有 user 文本时给随机负值）。
fn derive_session_id(messages: &[Value]) -> String {
    let seed = messages.iter().find_map(|message| {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "user" {
            return None;
        }
        match message.get("content") {
            Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        }
    });
    let number = match seed {
        Some(text) => {
            let digest = Sha256::digest(text.as_bytes());
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&digest[..8]);
            u64::from_be_bytes(bytes) & 0x7FFF_FFFF_FFFF_FFFF
        }
        None => {
            let mut bytes = [0u8; 8];
            if getrandom::getrandom(&mut bytes).is_err() {
                bytes = crate::server::logging::now_ms().to_be_bytes();
            }
            u64::from_be_bytes(bytes) & 0x7FFF_FFFF_FFFF_FFFF
        }
    };
    format!("-{number}")
}

/// OpenAI Chat 请求体 → `v1internal` 官方信封。
///
/// `model_key` 是已解析的目录键（`models::resolve` 的 `id`）；`project` 来自
/// 账号凭证（空则兜底 [`DEFAULT_PROJECT`]）；`effort` 是**已按模型声明集合
/// 收敛过**的思考档位（`"auto"` 是特例，映射为 `thinkingBudget: -1`）。
pub fn build_upstream_body(
    body: &Value,
    model_key: &str,
    project: &str,
    effort: Option<&str>,
) -> OutboundBody {
    let is_claude = super::models::is_claude(model_key);
    let is_image = super::models::is_image(model_key);

    // ── 工具名先扫一遍（历史 tool_calls 与声明要共用同一张占位表）────
    let mut used_names: Vec<String> = Vec::new();
    let (declarations, name_map) = build_tool_declarations(body, &mut used_names);
    let choice_none = body
        .get("tool_choice")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("none"));
    let tool_config = build_tool_config(body, is_claude, !declarations.is_empty(), &name_map);

    // ── 消息归一 ────────────────────────────────────────────────
    let mut converter = Converter {
        contents: Vec::new(),
        system_texts: Vec::new(),
        tool_names: HashMap::new(),
        name_map,
        // 声明已经占过名，历史里的 functionCall 复用对照表 / 已占位名
        used_names,
    };
    convert_messages(body, &mut converter);

    let system_instruction = converter.system_texts.join("\n\n");

    // ── generationConfig ────────────────────────────────────────
    let mut generation: Map<String, Value> = Map::new();
    if let Some(temperature) = body.get("temperature").and_then(Value::as_f64) {
        generation.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = body.get("top_p").and_then(Value::as_f64) {
        generation.insert("topP".to_string(), json!(top_p));
    }
    if let Some(top_k) = body.get("top_k").and_then(Value::as_f64) {
        generation.insert("topK".to_string(), json!(top_k));
    }
    // maxOutputTokens：**只有 Claude 保留**（非 Claude 一律删，spec B.2）
    if is_claude {
        let max_tokens = body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(Value::as_u64)
            .filter(|value| *value > 0);
        if let Some(max_tokens) = max_tokens {
            generation.insert(
                "maxOutputTokens".to_string(),
                json!(max_tokens.min(CLAUDE_MAX_OUTPUT_TOKENS)),
            );
        }
    }
    let mut thinking = false;
    if !is_image {
        if let Some(effort) = effort.map(str::trim).filter(|value| !value.is_empty()) {
            if !crate::server::core::model_rules::reasoning_is_off(effort) {
                thinking = true;
                let mut config = Map::new();
                config.insert("includeThoughts".to_string(), Value::Bool(true));
                if effort.eq_ignore_ascii_case("auto") {
                    // 动态思考（Manager 的 `"auto"` → thinkingBudget -1）
                    config.insert("thinkingBudget".to_string(), json!(-1));
                } else {
                    config.insert(
                        "thinkingLevel".to_string(),
                        Value::String(effort.to_string()),
                    );
                }
                generation.insert("thinkingConfig".to_string(), Value::Object(config));
            }
        }
    }
    apply_response_format(body, &mut generation);

    // ── 信封 ────────────────────────────────────────────────────
    let request_id = if is_image {
        None
    } else {
        Some(format!(
            "agent-{}",
            crate::server::core::upstream::request::new_request_id()
        ))
    };
    let session_id = if is_image {
        None
    } else {
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Some(derive_session_id(&messages))
    };

    let mut request = Map::new();
    request.insert("contents".to_string(), Value::Array(converter.contents));
    if !system_instruction.is_empty() {
        request.insert(
            "systemInstruction".to_string(),
            json!({ "role": "user", "parts": [{ "text": system_instruction }] }),
        );
    }
    // `tool_choice: none` 时连 tools 一起删（spec C.3）
    if !declarations.is_empty() && !choice_none {
        request.insert(
            "tools".to_string(),
            json!([{ "functionDeclarations": declarations }]),
        );
    }
    if let Some(tool_config) = tool_config {
        request.insert("toolConfig".to_string(), tool_config);
    }
    if !generation.is_empty() {
        request.insert("generationConfig".to_string(), Value::Object(generation));
    }
    if let Some(session_id) = session_id {
        request.insert("sessionId".to_string(), Value::String(session_id));
    }

    let mut payload = Map::new();
    payload.insert(
        "project".to_string(),
        Value::String(if project.trim().is_empty() {
            DEFAULT_PROJECT.to_string()
        } else {
            project.trim().to_string()
        }),
    );
    if let Some(request_id) = request_id {
        payload.insert("requestId".to_string(), Value::String(request_id));
    }
    payload.insert("request".to_string(), Value::Object(request));
    payload.insert("model".to_string(), Value::String(model_key.to_string()));
    payload.insert(
        "userAgent".to_string(),
        Value::String("antigravity".to_string()),
    );
    payload.insert(
        "requestType".to_string(),
        Value::String(if is_image { "image_gen" } else { "agent" }.to_string()),
    );

    OutboundBody {
        body: Value::Object(payload),
        thinking,
    }
}
