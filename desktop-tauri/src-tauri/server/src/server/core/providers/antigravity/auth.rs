//! Antigravity 的请求封装、项目发现与用户资料查询。
//!
//! ── 鉴权头 ──────────────────────────────────────────────────
//! 与 Accio 相反：**走标准 `Authorization: Bearer`**（Google 的老实形态）。
//! 项目发现类请求另有两个变体（spec §A.6）：
//!   - `loadCodeAssist`：host 固定 **prod**、**不带** `X-Goog-Api-Client`；
//!   - `onboardUser`：host 是推理基址、**带** `X-Goog-Api-Client`。
//! 两者都带 User-Agent（版本不能低于下限，见 `endpoints::UA_FLOOR_VERSION`）。

use serde_json::{json, Value};

use crate::server::core::auth_http::{send_raw, ApiResponse};
use crate::server::core::proxies::{resolve_account_proxy, ProxyResolution, ResolvedProxy};
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::credentials::{self, Credentials};
use super::endpoints;
use super::oauth;
use super::project;

/// 业务接口（非转发）的请求超时：与另外几家的 15 秒同量级
pub const REQUEST_TIMEOUT_MS: u64 = 15_000;

/// 一次出网请求（错误文案带家名，便于排障时区分链路）
pub async fn request(
    method: &str,
    url: &str,
    body: Option<&Value>,
    headers: &[(String, String)],
    proxy: Option<&ResolvedProxy>,
) -> Result<ApiResponse, GatewayError> {
    send_raw(method, url, body, headers, proxy, Some(REQUEST_TIMEOUT_MS))
        .await
        .map_err(|error| {
            if error.is_timeout() {
                GatewayError::with_status(504, "Antigravity 请求超时，请检查网络后重试")
                    .with_code("antigravity_transport")
            } else {
                GatewayError::with_status(502, "无法连接 Google 服务，请检查网络或账号代理设置")
                    .with_code("antigravity_transport")
            }
        })
}

/// POST + JSON body（项目发现类接口的约定；`extra` 追加额外请求头）
pub async fn post_json(
    url: &str,
    token: &str,
    body: &Value,
    extra: &[(&str, &str)],
    proxy: Option<&ResolvedProxy>,
) -> Result<ApiResponse, GatewayError> {
    let mut headers = endpoints::project_headers(token);
    for (key, value) in extra {
        headers.push(((*key).to_string(), (*value).to_string()));
    }
    request("POST", url, Some(body), &headers, proxy).await
}

/// 把响应翻成「有效 JSON 对象」，失败时给出人话。
pub fn payload(response: ApiResponse, action: &str) -> Result<Value, GatewayError> {
    if !response.ok {
        let hint = match response.status {
            401 | 403 => "，请确认凭证有效或重新登录",
            429 => "，请稍后重试",
            _ => "",
        };
        return Err(GatewayError::with_status(
            i32::from(response.status),
            format!("Antigravity {action}失败（HTTP {}）{hint}", response.status),
        ));
    }
    response.payload.filter(Value::is_object).ok_or_else(|| {
        GatewayError::with_status(502, format!("Antigravity {action}未返回有效 JSON 对象"))
    })
}

/// 账号级出口代理（解析失败按 400 报，不静默直连）
pub fn account_proxy(record: &Value) -> Result<Option<ResolvedProxy>, GatewayError> {
    match resolve_account_proxy(record.get("proxy")) {
        Some(ProxyResolution::Resolved(proxy)) => Ok(Some(proxy)),
        Some(ProxyResolution::Failed(reason)) => Err(GatewayError::with_status(400, reason)),
        None => Ok(None),
    }
}

/// `loadCodeAssist`：项目发现（prod 基址、不带 X-Goog-Api-Client，spec §A.6）
pub async fn load_code_assist(
    token: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    let body = json!({ "metadata": { "ideType": "ANTIGRAVITY" } });
    let response = post_json(&endpoints::load_code_assist_url(), token, &body, &[], proxy).await?;
    payload(response, "项目查询")
}

/// `onboardUser`：为账号开通默认档位（带 X-Goog-Api-Client，spec §A.6）
pub async fn onboard_user(
    token: &str,
    tier: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    let body = json!({
        "tier_id": tier,
        "metadata": {
            "ide_type": "ANTIGRAVITY",
            "ide_version": endpoints::ua_version(),
            "ide_name": "antigravity",
        }
    });
    let response = post_json(
        &endpoints::onboard_user_url(&endpoints::api_base()),
        token,
        &body,
        &[("X-Goog-Api-Client", endpoints::GOOG_API_CLIENT)],
        proxy,
    )
    .await?;
    payload(response, "项目开通")
}

/// 用户资料查询（best-effort：失败一律 None，不打断添加流程）
pub async fn fetch_userinfo(
    token: &str,
    proxy: Option<&ResolvedProxy>,
) -> Option<(String, String)> {
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {token}")),
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), endpoints::user_agent()),
    ];
    let response = send_raw(
        "GET",
        endpoints::USERINFO_URL,
        None,
        &headers,
        proxy,
        Some(REQUEST_TIMEOUT_MS),
    )
    .await
    .ok()?;
    if !response.ok {
        return None;
    }
    let payload = response.payload?;
    let email = credentials::text(&payload, &["email"]);
    let name = credentials::text(&payload, &["name", "given_name"]);
    if email.is_empty() && name.is_empty() {
        return None;
    }
    Some((email, name))
}

/// 「填写凭证」路径的快照（不触发任何网络请求）
pub fn snapshot(record: &Value) -> Result<Credentials, GatewayError> {
    Credentials::from_payload(record)
}

/// 「填写凭证」路径的入口：解析 payload → 补 access/资料 → 校验并取项目。
///
/// ── 为什么这里真打一次网络（loadCodeAssist）──────────────────
/// 手动粘贴的 token 无法在本地判真伪；`loadCodeAssist` 是**所有推理前必经**
/// 的一次轻请求（取 project），用它当校验：401/403 当场拒绝并给出人话，
/// 而不是等用户发第一条消息才在转发链路里看到 401。
/// 例外：payload 里已带 `projectId` 时，查询失败只记日志（用户自己知道凭证
/// 配了哪个项目，网络抖动不该拦住添加）。
pub async fn prepare_account(payload: &Value) -> Result<Credentials, GatewayError> {
    let mut credentials = Credentials::from_payload(payload)?;
    // 只有 refreshToken → 先换一次 access（换不动说明凭证不可用，当场拒绝）
    if credentials.access_token.is_empty() {
        let refreshed = oauth::refresh_access_token(&credentials.refresh_token, None).await?;
        credentials.access_token = refreshed.access_token;
        if credentials.expires_at.is_none() {
            credentials.expires_at = refreshed.expires_at;
        }
    }
    // email：payload 带 id_token 时先解出来（免得白打一次 userinfo）
    if credentials.email.is_empty() {
        if let Some(id_token) = payload
            .get("idToken")
            .or_else(|| payload.get("id_token"))
            .and_then(Value::as_str)
        {
            if let Some(email) = oauth::decode_jwt_claim(id_token, "email") {
                credentials.email = email.chars().take(320).collect();
            }
        }
    }
    if credentials.email.is_empty() || credentials.name.is_empty() {
        match fetch_userinfo(&credentials.access_token, None).await {
            Some((email, name)) => {
                if credentials.email.is_empty() && !email.is_empty() {
                    credentials.email = email.chars().take(320).collect();
                }
                if credentials.name.is_empty() && !name.is_empty() {
                    credentials.name = name.chars().take(100).collect();
                }
            }
            None => logging::verbose("[Antigravity]", "用户资料查询失败（不影响添加）"),
        }
    }
    let had_project = !credentials.project_id.is_empty();
    match project::ensure_project(&credentials.access_token, None).await {
        Ok(found) => {
            if !found.is_empty() {
                credentials.project_id = found;
            }
        }
        Err(error) => {
            if had_project {
                logging::verbose(
                    "[Antigravity]",
                    &format!("项目查询失败（沿用已提供的 projectId）: {}", error.message),
                );
            } else {
                return Err(error);
            }
        }
    }
    Ok(credentials)
}

/// 一个便于日志的账号摘要（绝不回显 token）
pub fn describe(credentials: &Credentials) -> String {
    let who = if !credentials.email.is_empty() {
        credentials.email.clone()
    } else if !credentials.project_id.is_empty() {
        credentials.project_id.clone()
    } else {
        "未知账号".to_string()
    };
    format!("{who}（Antigravity）")
}
