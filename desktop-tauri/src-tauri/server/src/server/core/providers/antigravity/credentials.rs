//! Antigravity 凭证格式：`accessToken` / `refreshToken` / `expiresAt` + 账号标识
//! （`email`、`projectId`）。
//!
//! ── 上游给的东西（OAuth token 端点 + loadCodeAssist）─────────
//! token 端点回 `{access_token, refresh_token, expires_in, id_token}`；
//! 项目发现回 `cloudaicompanionProject`（spec §A.2/A.6）。`refresh_token` 只在
//! **首次授权且 `access_type=offline` + `prompt=consent`** 时下发一次 ——
//! 丢了它就只能重新走一次浏览器授权，所以续期实现里「上游没回新 refresh_token
//! 时保留旧值」是硬规则（见 `refresh.rs`）。
//!
//! ── 身份用什么标识 ──────────────────────────────────────────
//! `email`：token 端点回的 `id_token`（JWT）中段能解出来，一次授权一条。
//! 账号层按 email 去重；email 缺失（例如手动粘贴只给了 token 串）时退回按
//! 凭证相等去重（见 `account_store::antigravity_accounts`）。
//!
//! ── `projectId` 为什么要落进凭证 ────────────────────────────
//! 推理信封的顶层 `project` 用的就是它；loadCodeAssist 每次都能取，但那是
//! 一次额外的远程请求。落进凭证后，转发路径零额外请求直接填信封。

use serde_json::{json, Value};

use crate::server::core::account_store::MAX_TOKEN_LENGTH;
use crate::server::errors::GatewayError;
use crate::server::logging;

/// 提前多久认为 token 该续期（spec §A.5：参考实现用 15 分钟量级）
pub const REFRESH_MARGIN_MS: i64 = 15 * 60 * 1000;

#[derive(Clone, Default)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: Option<i64>,
    /// 推理信封顶层 `project` 的取值（loadCodeAssist 的 `cloudaicompanionProject`）
    pub project_id: String,
    pub email: String,
    pub name: String,
}

impl Credentials {
    /// 从账号记录 / 添加请求 / OAuth 回调解析凭证。
    ///
    /// **故意不要求 accessToken**：手动添加常常只拿得到 refresh_token
    /// （CLIProxyAPI 的凭据文件与文档都以 refresh_token 为主），此时由
    /// `auth::prepare_account` 先换一次 access。要求的硬条件是
    /// 「accessToken 与 refreshToken 至少有一个」。
    pub fn from_payload(payload: &Value) -> Result<Self, GatewayError> {
        if !payload.is_object() {
            return Err(GatewayError::with_status(
                400,
                "Antigravity 账号内容必须是 JSON 对象",
            ));
        }
        let access_token = secret(payload, &["accessToken", "access_token", "token"])?;
        let refresh_token = secret(payload, &["refreshToken", "refresh_token"])?;
        if access_token.is_empty() && refresh_token.is_empty() {
            return Err(GatewayError::with_status(
                400,
                "缺少 Antigravity 凭证（accessToken 与 refreshToken 至少提供一个）",
            ));
        }
        let mut expires_at = ["expiresAt", "expires_at", "expiry", "expiry_timestamp", "expires"]
            .iter()
            .find_map(|key| timestamp(payload.get(*key)));
        if expires_at.is_none() {
            // CLIProxyAPI 凭据文件的形态：{expires_in, timestamp}（秒）
            let expires_in = ["expires_in", "expiresIn"]
                .iter()
                .find_map(|key| payload.get(*key).and_then(Value::as_i64))
                .filter(|value| *value > 0);
            let issued_at = ["timestamp", "timestamp_ms"]
                .iter()
                .find_map(|key| timestamp(payload.get(*key)))
                .unwrap_or_else(logging::now_ms);
            if let Some(seconds) = expires_in {
                if let Some(ttl) = seconds.checked_mul(1000) {
                    expires_at = issued_at.checked_add(ttl);
                }
            } else if let Some(expired) = payload.get("expired").and_then(Value::as_bool) {
                // CLIProxyAPI 的 `expired` 是布尔；true 表示已过期 → 立刻续期
                if expired {
                    expires_at = Some(0);
                }
            }
        }
        Ok(Self {
            access_token,
            refresh_token,
            expires_at,
            project_id: text(
                payload,
                &[
                    "projectId",
                    "project_id",
                    "cloudaicompanionProject",
                    "project",
                ],
            ),
            email: text(payload, &["email"]).chars().take(320).collect(),
            name: text(payload, &["name", "nickname"])
                .chars()
                .take(100)
                .collect(),
        })
    }

    /// 有刷新凭证 + 有到期时间，且快到点了
    pub fn expiring(&self) -> bool {
        self.can_refresh()
            && self
                .expires_at
                .is_some_and(|expiry| expiry <= logging::now_ms() + REFRESH_MARGIN_MS)
    }

    pub fn can_refresh(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    /// 落进账号记录的形态。空串按 null 落（账号层的合并规则是「空值不覆盖」，
    /// 缺项不该把既有值洗成空）。
    pub fn to_value(&self) -> Value {
        let text_or_null = |value: &str| -> Value {
            if value.is_empty() {
                Value::Null
            } else {
                Value::String(value.to_string())
            }
        };
        json!({
            "accessToken": text_or_null(&self.access_token),
            "refreshToken": text_or_null(&self.refresh_token),
            "expiresAt": self.expires_at,
            "projectId": text_or_null(&self.project_id),
            "email": text_or_null(&self.email),
            "name": text_or_null(&self.name),
        })
    }
}

/// 取一个文本字段（空串按缺失处理）
pub fn text(payload: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| match payload.get(*key) {
            Some(Value::String(value)) if !value.trim().is_empty() => {
                Some(value.trim().to_string())
            }
            Some(Value::Number(value)) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

/// 取一个密钥字段：去 `Bearer ` 前缀、限长、拒绝控制字符。
pub fn secret(payload: &Value, keys: &[&str]) -> Result<String, GatewayError> {
    let mut value = keys
        .iter()
        .find_map(|key| {
            payload
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("");
    if value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
    {
        value = value[7..].trim();
    }
    if value.len() > MAX_TOKEN_LENGTH || value.chars().any(char::is_control) {
        return Err(GatewayError::with_status(
            400,
            "Antigravity 凭证过长或包含非法控制字符",
        ));
    }
    Ok(value.to_string())
}

/// 时间戳（毫秒）；接受秒级数字与 RFC3339 字符串。0 / 负数 / 解析不出 → None。
pub fn timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    let number = value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok());
    if let Some(number) = number.filter(|value| *value > 0) {
        return if number < 100_000_000_000 {
            number.checked_mul(1000)
        } else {
            Some(number)
        };
    }
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|time| time.timestamp_millis())
}
