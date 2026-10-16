//! Antigravity 网页登录：Google OAuth 2.0 授权码。
//!
//! ── 协议（spec §A.1–A.4）────────────────────────────────────
//! ```text
//! ① 打开 https://accounts.google.com/o/oauth2/v2/auth?access_type=offline
//!        &client_id=…&prompt=consent&redirect_uri=http://localhost:51121/oauth-callback
//!        &response_type=code&scope=<5 条>&state=antigravity-<random>
//! ② 用户登录完成 → Google 302 到 localhost:51121/oauth-callback?code=…&state=…
//! ③ POST https://oauth2.googleapis.com/token  (form)
//!        {code, client_id, client_secret, redirect_uri, grant_type=authorization_code}
//!        → {access_token, refresh_token, expires_in, id_token}
//! ④ GET  https://www.googleapis.com/oauth2/v2/userinfo (Bearer) → {email, name}
//! ⑤ POST {prod}/v1internal:loadCodeAssist（取 project，见 `project.rs`）
//! ```
//!
//! ── 回调落在哪：**不是本进程** ───────────────────────────────
//! `redirect_uri` 是 Google 侧登记死的 `http://localhost:51121/oauth-callback`
//! （两个参考实现逐字相同），本网关不在 51121 上监听。三条路径把回调送回
//! 本进程的 `/oauth/callback`：
//!   - **桌面壳**：内嵌窗口/系统浏览器导航到 51121 时，壳把地址重写成网关的
//!     `/oauth/callback`（见 `src-tauri/src/login.rs` 的回调拦截）；
//!   - **Web 面板**：用户手动把地址栏里 localhost:51121 的主机改成网关地址
//!     （CodeArts 同款的「改地址栏」流程）；
//!   - 路由收尾见 `api::session::login_codearts_callback` 对
//!     `state` 前缀 `antigravity-` 的分派。
//!
//! ── 无 PKCE ─────────────────────────────────────────────────
//! 与 Accio 不同：Google 这条链路的 `client_secret` 是公开客户端里的固定
//! 常量（两个参考实现逐字相同），授权码换 token 不需要 code_verifier。
//! 防伪靠 `state`：由登录任务生成、回调时前缀 + 任务表双重校验。
//!
//! ── panic=abort ────────────────────────────────────────────
//! 本文件在登录链路上，绝不 unwrap/expect/panic。

use std::collections::HashMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;

use crate::server::core::proxies::ResolvedProxy;
use crate::server::core::upstream::request::new_request_id;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::auth;
use super::credentials::{self, Credentials};
use super::endpoints;
use super::project;

/// 登录 state 的前缀（`/oauth/callback` 路由据此分派，见模块头）
pub const STATE_PREFIX: &str = "antigravity-";

/// 发起一轮登录：返回 `(authorize_url, state)`。
///
/// `state` 由登录任务生成后交回（任务表是唯一事实来源，这里不做 pending 表）。
pub fn begin_login() -> (String, String) {
    let state = format!("{STATE_PREFIX}{}", new_request_id().replace('-', ""));
    (authorize_url(&state), state)
}

/// 授权页地址（参数与参考实现逐字一致，spec §A.1）
pub fn authorize_url(state: &str) -> String {
    format!(
        "{}?access_type=offline&client_id={}&prompt=consent&redirect_uri={}&response_type=code&scope={}&state={}",
        endpoints::AUTH_URL,
        urlencode(endpoints::CLIENT_ID),
        urlencode(endpoints::CALLBACK_URL),
        urlencode(&endpoints::scope_string()),
        urlencode(state),
    )
}

/// query 参数编码（与 `core::auth` / Accio 同口径：只转义必须转义的字符）
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 校验回调 query 并取出授权码（`error=` 参数按人话拒绝）。
pub fn validate_callback(query: &HashMap<String, String>) -> Result<String, GatewayError> {
    if let Some(error) = query.get("error").filter(|value| !value.trim().is_empty()) {
        return Err(GatewayError::with_status(
            400,
            format!("授权被拒绝（{error}）"),
        ));
    }
    let code = query.get("code").map(String::as_str).unwrap_or("").trim();
    if code.is_empty() {
        return Err(GatewayError::with_status(400, "回调没有携带授权码"));
    }
    if code.len() > 8192 {
        return Err(GatewayError::with_status(400, "回调参数超长，已拒绝"));
    }
    Ok(code.to_string())
}

/// 用授权码换凭证（并在登录路径上尽力把 project 取回来）。
pub async fn exchange_code(
    code: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<Credentials, GatewayError> {
    let data = post_form(
        endpoints::TOKEN_URL,
        &[
            ("code", code),
            ("client_id", endpoints::CLIENT_ID),
            ("client_secret", endpoints::CLIENT_SECRET),
            ("redirect_uri", endpoints::CALLBACK_URL),
            ("grant_type", "authorization_code"),
        ],
        proxy,
    )
    .await?;
    let access_token = credentials::secret(&data, &["access_token"])?;
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            502,
            "Antigravity 换码响应缺少 access_token",
        ));
    }
    let refresh_token = credentials::secret(&data, &["refresh_token"])?;
    if refresh_token.is_empty() {
        // `prompt=consent` + `access_type=offline` 正常都会给 refresh_token；
        // 缺了不阻断本次登录（access 还能用一小时），但没有续期能力
        logging::verbose(
            "[Antigravity]",
            "换码响应没有 refresh_token（续期需要重新授权）",
        );
    }
    let expires_at = data
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| seconds.checked_mul(1000))
        .map(|ttl| logging::now_ms().saturating_add(ttl));
    // email/name：id_token（JWT）里直接有 email；拿不到再问 userinfo
    let mut credentials = Credentials {
        access_token,
        refresh_token,
        expires_at,
        project_id: String::new(),
        email: data
            .get("id_token")
            .and_then(Value::as_str)
            .and_then(|token| decode_jwt_claim(token, "email"))
            .map(|email| email.chars().take(320).collect())
            .unwrap_or_default(),
        name: String::new(),
    };
    if credentials.email.is_empty() || credentials.name.is_empty() {
        if let Some((email, name)) = auth::fetch_userinfo(&credentials.access_token, proxy).await {
            if credentials.email.is_empty() && !email.is_empty() {
                credentials.email = email.chars().take(320).collect();
            }
            if credentials.name.is_empty() && !name.is_empty() {
                credentials.name = name.chars().take(100).collect();
            }
        }
    }
    // 项目发现：登录路径尽力而为（失败不阻断落账号 —— 信封有兜底 project）
    match project::ensure_project(&credentials.access_token, proxy).await {
        Ok(project) => {
            if !project.is_empty() {
                credentials.project_id = project;
            }
        }
        Err(error) => logging::verbose(
            "[Antigravity]",
            &format!("登录后项目查询失败（不影响落账号）: {}", error.message),
        ),
    }
    Ok(credentials)
}

/// 续期结果（`refresh_access_token` 的产出；refresh_token 一般不变，不带）
pub struct RefreshedToken {
    pub access_token: String,
    pub expires_at: Option<i64>,
}

/// 用 refresh_token 换新的 access_token（`POST {TOKEN_URL}`，form）。
pub async fn refresh_access_token(
    refresh_token: &str,
    proxy: Option<&ResolvedProxy>,
) -> Result<RefreshedToken, GatewayError> {
    let data = post_form(
        endpoints::TOKEN_URL,
        &[
            ("client_id", endpoints::CLIENT_ID),
            ("client_secret", endpoints::CLIENT_SECRET),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
        proxy,
    )
    .await?;
    let access_token = credentials::secret(&data, &["access_token"])?;
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            502,
            "Antigravity 续期响应缺少 access_token，旧凭证未被覆盖",
        ));
    }
    let expires_at = data
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| seconds.checked_mul(1000))
        .map(|ttl| logging::now_ms().saturating_add(ttl));
    Ok(RefreshedToken {
        access_token,
        expires_at,
    })
}

/// form 编码的 POST（Google 令牌端点只收 `application/x-www-form-urlencoded`）。
///
/// 项目里 `auth_http::send_raw` 只发 JSON body，因此这一条自建 —— 与
/// 其它家「表单请求」的做法一致（确实只有令牌端点用得到）。
async fn post_form(
    url: &str,
    fields: &[(&str, &str)],
    proxy: Option<&ResolvedProxy>,
) -> Result<Value, GatewayError> {
    let client = crate::server::core::egress::client_for(proxy);
    let builder = client
        .post(url)
        .form(fields)
        .header("Accept", "application/json");
    let budget =
        std::time::Duration::from_millis(crate::server::config::timeout_settings().headers_ms());
    let response = match tokio::time::timeout(budget, builder.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return Err(GatewayError::with_status(
                502,
                format!(
                    "无法连接 Google 令牌服务: {}",
                    crate::server::core::egress::describe_error_detail(&error)
                ),
            )
            .with_code("antigravity_transport"))
        }
        Err(_elapsed) => {
            return Err(GatewayError::with_status(
                504,
                "Antigravity 令牌请求超时，请检查网络后重试",
            )
            .with_code("antigravity_transport"))
        }
    };
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let error_code = payload.get("error").and_then(Value::as_str).unwrap_or("");
        let detail = payload
            .get("error_description")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                payload
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(300).collect());
        // invalid_grant = refresh_token 已失效 → 401（提示重新登录）
        let mapped = if error_code == "invalid_grant" || status == 401 {
            401
        } else {
            502
        };
        return Err(GatewayError::with_status(
            mapped,
            format!("Google 令牌接口返回 {status}: {detail}"),
        )
        .with_optional_code(Some(i64::from(status))));
    }
    if !payload.is_object() {
        return Err(GatewayError::with_status(
            502,
            "Google 令牌接口未返回有效 JSON",
        ));
    }
    Ok(payload)
}

/// 解出 JWT（id_token）中段的某个声明（不验证签名 —— 只用于取 email 展示；
/// 凭证的真实性由后续 loadCodeAssist 的 401/403 校验兜住）。
pub fn decode_jwt_claim(id_token: &str, claim: &str) -> Option<String> {
    let payload_segment = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload_segment)
        .or_else(|_| URL_SAFE_NO_PAD.decode(payload_segment.trim_end_matches('=')))
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get(claim)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}
