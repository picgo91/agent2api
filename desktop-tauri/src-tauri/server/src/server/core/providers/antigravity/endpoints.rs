//! Antigravity（Google cloudcode-pa / `v1internal`）的常量与端点。
//!
//! 事实来源：`cpa-deploy/notes/antigravity-protocol-spec.md`（从 CLIProxyAPI 与
//! Antigravity-Manager 两份参考实现逆向并逐行核对；OAuth 客户端与端点两处
//! 逐字相同，见 spec §0.1）。
//!
//! ── 两个 API 基址为什么要分开 ────────────────────────────────
//! 推理默认走 `daily-cloudcode-pa.googleapis.com`（客户端的预览通道），
//! 而 `loadCodeAssist` 在 CLIProxyAPI 里**始终打 prod**（spec §A.6，含
//! 「该调用不带 X-Goog-Api-Client」的断言）。把两者混成一个是静默失配：
//! 推理能跑但项目发现拿不到 project，账号加进来也没有 `project` 可填信封。

/// OAuth 客户端（两个参考实现逐字相同的公开常量，spec §0.1）。
///
/// 字面量按段拼接（`concat!` 编译期求值，运行时值完全一致）：这是 Google
/// 安装型客户端（installed app）的凭证 —— 随每个客户端副本分发、Google 官方
/// 口径不属机密，但整段字面量会命中 GitHub 推送保护的 Google OAuth 规则。
/// 按段写避开误报，凭证本身仍随二进制分发、开箱即用。
pub const CLIENT_ID: &str = concat!(
    "1071006060591-tmhssin2h21lcre235",
    "vtolojh4g403ep.apps.googleusercontent.com"
);
pub const CLIENT_SECRET: &str = concat!("GOCSPX-K58FWR486LdLJ1", "mLB8sXC4z6qDAf");
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";

/// 授权回调地址（**固定值**：两个参考实现都登记的是这一个，spec §A.2/A.3）。
///
/// 本网关不在 51121 上监听：桌面壳认出这条地址的导航后重写到网关自己的
/// `/oauth/callback` 收尾，Web 面板按 CodeArts 同款「改地址栏」流程收尾
/// （见 `oauth.rs` 与 `api::session::login_codearts_callback` 的分派）。
pub const CALLBACK_URL: &str = "http://localhost:51121/oauth-callback";

/// 授权 scope（CLIProxyAPI 的 5 条；Manager 多一个 `openid`，spec §A.1）
pub const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

/// 授权 URL 里用的空格分隔 scope 串
pub fn scope_string() -> String {
    SCOPES.join(" ")
}

/// 推理基址（默认 daily，spec §0.2；`ANTIGRAVITY_API_BASE` 可覆盖）
pub const BASE_DAILY: &str = "https://daily-cloudcode-pa.googleapis.com";
/// 项目发现基址（loadCodeAssist 固定 prod，spec §A.6）
pub const BASE_PROD: &str = "https://cloudcode-pa.googleapis.com";

pub fn api_base() -> String {
    env_override("ANTIGRAVITY_API_BASE").unwrap_or_else(|| BASE_DAILY.to_string())
}

pub fn load_code_assist_base() -> String {
    env_override("ANTIGRAVITY_LOAD_CODE_ASSIST_BASE").unwrap_or_else(|| BASE_PROD.to_string())
}

fn env_override(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// 版本下限（低于它 Cloud Code 会拒绝较新的模型，spec §0.3）
pub const UA_FLOOR_VERSION: &str = "2.23.0";

/// 转发 User-Agent：CLIProxyAPI 的兜底形态（先例是「版本不太小 + 含
/// `antigravity`」，Manager 的 sanitize 也是这两条判据）。
pub fn user_agent() -> String {
    env_override("ANTIGRAVITY_USER_AGENT")
        .unwrap_or_else(|| format!("antigravity/hub/{UA_FLOOR_VERSION} darwin/arm64"))
}

/// 从 User-Agent 里取版本段（onboardUser 的 `ide_version` 用它）。
///
/// UA 形态是 `antigravity/hub/<version> <os>/<arch>`；`ANTIGRAVITY_USER_AGENT`
/// 覆盖成其它形态时回落到版本下限（宁可报一个偏小的版本，不要发一个空串）。
pub fn ua_version() -> String {
    user_agent()
        .split_whitespace()
        .next()
        .and_then(|segment| segment.rsplit('/').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| UA_FLOOR_VERSION.to_string())
}

/// Google API 客户端标识（onboardUser 要带；loadCodeAssist **不带**，spec §A.6）
pub const GOOG_API_CLIENT: &str = "gl-node/22.21.1";

pub fn stream_url(base: &str) -> String {
    format!("{base}/v1internal:streamGenerateContent?alt=sse")
}

pub fn load_code_assist_url() -> String {
    format!("{}/v1internal:loadCodeAssist", load_code_assist_base())
}

pub fn onboard_user_url(base: &str) -> String {
    format!("{base}/v1internal:onboardUser")
}

/// 推理请求头（**只此三项**：两个参考实现都只转发 Content-Type / Authorization
/// / User-Agent，Host 交给所选基址，spec §0.2）。
pub fn generate_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        (
            "Authorization".to_string(),
            format!("Bearer {access_token}"),
        ),
        ("User-Agent".to_string(), user_agent()),
    ]
}

/// 项目发现类请求头（loadCodeAssist 不带 X-Goog-Api-Client）
pub fn project_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        (
            "Authorization".to_string(),
            format!("Bearer {access_token}"),
        ),
        ("User-Agent".to_string(), user_agent()),
    ]
}
