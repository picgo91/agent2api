//! Antigravity 上游 SSE 的解析与翻译（`v1internal` 信封 → OpenAI Chat chunk）。
//!
//! ── 上游的流长什么样（spec C.1）──────────────────────────────
//! ```text
//! data: {"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"你"}]}}]},"traceId":""}
//! data: {"response":{"candidates":[{"content":{"parts":[{"thought":true,"text":"想一想…"}]}}]}}
//! data: {"response":{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a.txt"}}}]}}]}}
//! data: {"response":{"candidates":[{"content":{"parts":[{"text":"好"}]},"finishReason":"STOP"}],"usageMetadata":{…}},"traceId":""}
//! data: {"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"…"}}
//! data: [DONE]
//! ```
//!
//! ── 三处与 OpenAI 不同，必须在这里处理 ───────────────────────
//!   1. **帧外层是 `{"response": …}` 包裹**（内层才是 GenerateContentResponse），
//!      心跳形态是 `{}` / `{"response":{}}` / 空 candidates —— 都不能当作
//!      「流已开始」（spec C.1 的 `hasAntigravityResponsePayload`）；
//!   2. **思考是 part 上的标记**（`{"thought":true,"text":…}`）、**工具参数的
//!      是对象**（`args`），各翻成 `reasoning_content` 与 tool_calls；
//!   3. **终帧判定是「`finishReason` 与 `usageMetadata` 同帧」**（spec C.4）——
//!      只有 usage 的帧不是结束；`finishReason` 要缓存到终帧才下发。
//!
//! ── 一个不做的事 ────────────────────────────────────────────
//! `inlineData`（图像模型产出的图片）本网关不下发（OpenAI 兼容层没有稳定的
//! 图像增量形态）；图像模型走 `image_gen` 时以文本兜底。见 `chat.rs` 的说明。
//!
//! ── panic=abort ────────────────────────────────────────────
//! 本文件在转发热路径上，绝不 unwrap/expect/panic：取值一律走 Option 链。

use serde_json::{json, Value};

use super::protocol;

/// 一帧里解析出的 part
#[derive(Clone, PartialEq, Debug)]
pub enum Part {
    /// 正文
    Text(String),
    /// 思考（`thought: true` 的文本 part）
    Thought(String),
    /// 工具调用（参数是完整 JSON 对象串 —— Antigravity 不分片，见模块头）
    FunctionCall { name: String, args: String },
    /// 其它（thoughtSignature、inlineData 等）：转发热路径不消费
    Other,
}

/// 一帧（`data:` 行）解析后的形态
#[derive(Clone, Default, Debug)]
pub struct Frame {
    pub parts: Vec<Part>,
    /// `finishReason` 原文（可能为空；大写化在翻译层做）
    pub finish_reason: String,
    /// usage 对象（已翻成 OpenAI 字段名）
    pub usage: Option<Value>,
    /// 错误帧的文案（`{"error": {…}}`）
    pub error: Option<String>,
}

impl Frame {
    /// 错误帧（HTTP 上的额度 / 鉴权问题都以错误帧出现）
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    /// 错误文案（分类用）
    pub fn error_text(&self) -> String {
        self.error.clone().unwrap_or_default()
    }

    /// 终帧：`finishReason` 与 usage **同帧**（spec C.4）
    pub fn terminal(&self) -> bool {
        !self.finish_reason.is_empty() && self.usage.is_some()
    }

    /// 这一帧算不算「流已经开始」（预读停下与换号的判据；心跳不算）
    pub fn has_payload(&self) -> bool {
        !self.parts.is_empty() || self.usage.is_some() || !self.finish_reason.is_empty()
    }
}

/// 双写取值：先 camelCase 再 snake_case
fn field<'a>(value: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    value
        .get(camel)
        .or_else(|| value.get(snake))
        .filter(|item| !item.is_null())
}

fn text_field(value: &Value, camel: &str, snake: &str) -> String {
    field(value, camel, snake)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 单个 part → `Part`
fn parse_part(part: &Value) -> Part {
    if let Some(function_call) = part
        .get("functionCall")
        .or_else(|| part.get("function_call"))
    {
        let name = text_field(function_call, "name", "name");
        // `args` 是对象（不是字符串）；字符串形态（防御）原样保留
        let args = match function_call.get("args") {
            Some(Value::String(text)) => text.clone(),
            Some(value) if !value.is_null() => value.to_string(),
            _ => "{}".to_string(),
        };
        return Part::FunctionCall { name, args };
    }
    if let Some(text) = part.get("text").and_then(Value::as_str) {
        let thought = part
            .get("thought")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return if thought {
            Part::Thought(text.to_string())
        } else {
            Part::Text(text.to_string())
        };
    }
    Part::Other
}

/// usage → OpenAI 字段名（spec C.4 的映射）
fn parse_usage(usage: &Value) -> Value {
    let number = |camel: &str, snake: &str| -> i64 {
        field(usage, camel, snake)
            .and_then(|value| value.as_i64().or_else(|| value.as_f64().map(|f| f as i64)))
            .unwrap_or(0)
    };
    let prompt = number("promptTokenCount", "prompt_token_count");
    let candidates = number("candidatesTokenCount", "candidates_token_count");
    let thoughts = number("thoughtsTokenCount", "thoughts_token_count");
    let total = {
        let explicit = number("totalTokenCount", "total_token_count");
        if explicit > 0 {
            explicit
        } else {
            prompt + candidates + thoughts
        }
    };
    let cached = number("cachedContentTokenCount", "cached_content_token_count");
    let mut usage = json!({
        "prompt_tokens": prompt,
        // 思考 token 计入输出（OpenAI 口径：思考也是模型产出的 token）
        "completion_tokens": candidates + thoughts,
        "total_tokens": total,
    });
    if thoughts > 0 {
        usage["completion_tokens_details"] = json!({ "reasoning_tokens": thoughts });
    }
    if cached > 0 {
        usage["prompt_tokens_details"] = json!({ "cached_tokens": cached });
    }
    usage
}

/// 一帧 `data:` 的 JSON 文本 → [`Frame`]。
///
/// 心跳 / `[DONE]` / 非 JSON 行返回 None（都按「无内容」跳过）。
pub fn parse_frame(data: &str) -> Option<Frame> {
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" || trimmed == "[done]" {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    if !value.is_object() {
        return None;
    }
    // 错误帧：`{"error": {…}}`（spec C.2）
    if let Some(error) = value.get("error").filter(|item| item.is_object()) {
        let mut text = String::new();
        if let Some(code) = field(error, "code", "code") {
            if let Some(number) = code.as_i64() {
                text.push_str(&format!("[{number}] "));
            } else if let Some(raw) = code.as_str() {
                text.push_str(&format!("[{raw}] "));
            }
        }
        if let Some(status) = error.get("status").and_then(Value::as_str) {
            if !status.is_empty() {
                text.push_str(&format!("[{status}] "));
            }
        }
        if let Some(message) = error.get("message").and_then(Value::as_str) {
            text.push_str(message);
        }
        if text.is_empty() {
            text = "上游返回未知错误".to_string();
        }
        return Some(Frame {
            error: Some(text),
            ..Frame::default()
        });
    }
    // 内层：标准是 `{"response": {…}}`；裸内层（防御）也接受
    let inner = value
        .get("response")
        .filter(|item| item.is_object())
        .unwrap_or(&value);
    let mut frame = Frame::default();
    if let Some(candidate) = inner
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
    {
        if let Some(parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                frame.parts.push(parse_part(part));
            }
        }
        frame.finish_reason = text_field(candidate, "finishReason", "finish_reason");
    }
    if let Some(usage) = field(inner, "usageMetadata", "usage_metadata")
        .or_else(|| field(inner, "cpaUsageMetadata", "cpa_usage_metadata"))
    {
        if usage.is_object() {
            frame.usage = Some(parse_usage(usage));
        }
    }
    // 心跳（`{}` / `{"response":{}}` / 空 candidates）不是「流已开始」
    if !frame.has_payload() {
        return None;
    }
    Some(frame)
}

/// 一个 SSE 帧（下发用）：`data: {json}\n\n`
pub fn sse_frame(value: &Value) -> String {
    format!("data: {value}\n\n")
}

/// 流结束帧
pub fn sse_done() -> String {
    "data: [DONE]\n\n".to_string()
}

/// 逐行缓冲（跨分片半行拼接）：SSE 的 `data:` 行可能被切在任意位置
#[derive(Default)]
pub struct LineBuffer {
    pending: String,
}

impl LineBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一段字节，产出**完整行**
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.pending.push_str(chunk);
        let mut lines = Vec::new();
        while let Some(index) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=index).collect();
            let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
            lines.push(trimmed);
        }
        lines
    }

    /// 取走还没成行的尾巴（首帧预读换手时必须做；见 Accio 同名方法的说明）
    pub fn take_pending(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}

/// 面向客户端的增量
#[derive(Clone, Debug)]
pub enum Delta {
    Content(String),
    Reasoning(String),
    ToolCall {
        index: i64,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
}

/// 累积中的工具调用。
///
/// Antigravity 的 functionCall 参数是**完整对象、不分片**（spec C.4 的
/// `args.Raw`），所以每个 part 就是一次独立调用 —— 不学 Accio 那种「按名字
/// 续写」的聚合（那是它的上游会分片才需要的）。
#[derive(Clone, Default)]
struct ToolAccum {
    id: String,
    name: String,
    args: String,
}

/// 上游帧 → 客户端帧 的翻译状态（流式与非流式共用同一套累积规则）。
pub struct Translator {
    pub response_id: String,
    created: i64,
    model_name: String,
    tool_state: Vec<ToolAccum>,
    role_sent: bool,
    usage: Option<Value>,
    finish_reason: Option<String>,
    content: String,
    reasoning: String,
    /// 收到过任何工具调用（决定收尾的 finish_reason 是 tool_calls 还是 stop）
    saw_tool_call: bool,
}

impl Translator {
    pub fn new(response_id: String, model_name: String) -> Self {
        Self {
            response_id,
            created: crate::server::logging::now_ms() / 1000,
            model_name,
            tool_state: Vec::new(),
            role_sent: false,
            usage: None,
            finish_reason: None,
            content: String::new(),
            reasoning: String::new(),
            saw_tool_call: false,
        }
    }

    /// 合成一次工具调用的 id：`{name}-{unixNano}-{counter}`（spec C.4）
    fn tool_id(&mut self, name: &str) -> String {
        let counter = self.tool_state.len() + 1;
        let nano = crate::server::logging::now_ms().saturating_mul(1_000_000);
        format!("{name}-{nano}-{counter}")
    }

    /// 一帧 → 若干 delta（顺序即下发顺序）
    pub fn consume(
        &mut self,
        frame: &Frame,
        telemetry: Option<&std::sync::Arc<crate::server::core::upstream::usage::RequestTelemetry>>,
    ) -> Vec<Delta> {
        let mut out = Vec::new();
        if let Some(usage) = &frame.usage {
            self.usage = Some(usage.clone());
            if let Some(telemetry) = telemetry {
                telemetry.report_usage(usage);
            }
        }
        if !frame.finish_reason.is_empty() {
            self.finish_reason = Some(protocol::finish_reason(&frame.finish_reason).to_string());
        }
        for part in &frame.parts {
            match part {
                Part::Text(text) => {
                    if !text.is_empty() {
                        self.content.push_str(text);
                        out.push(Delta::Content(text.clone()));
                    }
                }
                Part::Thought(text) => {
                    if !text.is_empty() {
                        self.reasoning.push_str(text);
                        out.push(Delta::Reasoning(text.clone()));
                    }
                }
                Part::FunctionCall { name, args } => {
                    if name.is_empty() {
                        continue;
                    }
                    let id = self.tool_id(name);
                    self.saw_tool_call = true;
                    let index = self.tool_state.len() as i64;
                    self.tool_state.push(ToolAccum {
                        id: id.clone(),
                        name: name.clone(),
                        args: args.clone(),
                    });
                    out.push(Delta::ToolCall {
                        index,
                        id: Some(id),
                        name: Some(name.clone()),
                        arguments: Some(args.clone()),
                    });
                }
                Part::Other => {}
            }
        }
        out
    }

    /// 流式的 role 帧是否该发（第一次产出 delta 前补一帧 role）
    pub fn take_role_frame(&mut self) -> bool {
        if self.role_sent {
            return false;
        }
        self.role_sent = true;
        true
    }

    pub fn model_out(&self) -> String {
        self.model_name.clone()
    }

    pub fn response_id(&self) -> String {
        self.response_id.clone()
    }

    pub fn created(&self) -> i64 {
        self.created
    }

    /// 收尾的 finish_reason：上游给了就用它，带工具调用时修正为 tool_calls
    /// （上游发 STOP 与工具调用并存，spec C.4 的 `resolveOpenAIFinishReason`）
    pub fn final_finish_reason(&self) -> String {
        if self.saw_tool_call {
            return match self.finish_reason.as_deref() {
                Some("length") | Some("content_filter") => {
                    self.finish_reason.clone().unwrap_or_default()
                }
                _ => "tool_calls".to_string(),
            };
        }
        self.finish_reason
            .clone()
            .unwrap_or_else(|| "stop".to_string())
    }

    /// 收尾的工具调用（非流式聚合用）
    pub fn final_tool_calls(&self) -> Vec<Value> {
        self.tool_state
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.name.is_empty())
            .map(|(index, entry)| {
                let arguments = if entry.args.is_empty() {
                    "{}".to_string()
                } else {
                    entry.args.clone()
                };
                json!({
                    "index": index,
                    "id": if entry.id.is_empty() { format!("call_{index}") } else { entry.id.clone() },
                    "type": "function",
                    "function": { "name": entry.name, "arguments": arguments },
                })
            })
            .collect()
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    pub fn usage(&self) -> Option<Value> {
        self.usage.clone()
    }
}
