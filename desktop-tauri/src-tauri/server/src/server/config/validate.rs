//! 启动时的环境变量校验（headless 形态）。
//!
//! ── 为什么要有这一层 ──────────────────────────────────────────
//! 配置读取是**刻意宽容**的（见 `config/parse.rs` 的模块头）：非法值一律回落
//! 默认，绝不因为一个手滑就让服务起不来。这对「读文件」是对的 —— 但环境变量
//! 是**部署者当场写的**，写错时静默回落反而是最坏的结果：
//!   · `AIAPI_PROXY_PORT=30650`（多打一位）会静默跑回 3065，用户以为在 30650 上等；
//!   · `AIAPI_ADMIN_USER` 填了、密码忘填 → 面板认证静默不启用，用户以为「已经要登录了」；
//!   · `AIAPI_CAPTCHA_ENABLED=yes` → 被当成「开启」（非 0 即开），与字面预期相反。
//! 这些都属于「配置明显有错、且错法会误导人」，应当在启动时**明确指出**：
//! 致命的（端口非法）直接拒绝启动；非致命的（账号配对不全、取值可疑）打印警告。
//!
//! ── 与 aiapi-server.rs 的分工 ────────────────────────────────
//! 入口仍负责**解析**（拿到 host / port / panel_port 的值），本模块只做
//! 「这些原始字符串合不合法、组合起不合理」的判定，并集中在一处、可单测。

use std::env;

/// 校验结果：`Err` = 致命（调用方应拒绝启动并打印原因）；
/// `Ok(warnings)` = 可继续，但有这些需要提醒部署者的问题。
pub fn validate_env() -> Result<Vec<String>, String> {
    let mut warnings: Vec<String> = Vec::new();

    // ── 端口 ────────────────────────────────────────────────
    // 主端口非法 = 致命。既有实现里 `env_port` 把非法值当「未设置」静默回落 3065，
    // 正是我们要消除的误导。
    let main_port = read_any(&[
        "AIAPI_PROXY_PORT",
        "AGENT2API_PROXY_PORT",
        "WORKBUDDY_PROXY_PORT",
    ]);
    let main_port = match main_port {
        Some(raw) => Some(parse_port(&raw, "AIAPI_PROXY_PORT")?),
        None => None,
    };
    if let Some(raw) = read_any(&["AIAPI_PANEL_PORT", "AGENT2API_PANEL_PORT"]) {
        let panel = parse_port(&raw, "AIAPI_PANEL_PORT")?;
        if Some(panel) == main_port {
            return Err("AIAPI_PANEL_PORT 与网关端口相同：分端口部署要求两者不同".to_string());
        }
    }

    // ── 管理员账号配对 ──────────────────────────────────────
    // 只设了一半 = 面板认证**不会**启用（见 access::env_admin 的 `(Some, Some)` 分支）。
    // 这种「以为设了、其实没设」必须提醒，否则部署者会以为面板已经受保护。
    let user = read_any(&["AIAPI_ADMIN_USER", "AGENT2API_ADMIN_USER"]);
    let secret = read_any(&[
        "AIAPI_ADMIN_PASSWORD",
        "AGENT2API_ADMIN_PASSWORD",
        "AIAPI_ADMIN_PASSWORD_HASH",
        "AGENT2API_ADMIN_PASSWORD_HASH",
    ]);
    match (user.is_some(), secret.is_some()) {
        (true, false) => warnings.push(
            "AIAPI_ADMIN_USER 已设置但缺少密码：请同时设置 AIAPI_ADMIN_PASSWORD 或 AIAPI_ADMIN_PASSWORD_HASH，否则面板认证不会启用".to_string(),
        ),
        (false, true) => warnings.push(
            "已设置管理员密码但缺少 AIAPI_ADMIN_USER：两者需同时设置才会启用面板认证".to_string(),
        ),
        _ => {}
    }

    // ── CAPTCHA 开关取值 ────────────────────────────────────
    // 读取侧口径是「非 0 即开」（见 config/parse.rs env_captcha_enabled）。
    // 填了 `true` / `yes` / `on` 这类看着像布尔的值会被当成「开」—— 与直觉相符，
    // 但 `false` / `no` / `off` **也是非 0 → 同样被当成开**，这才是反直觉的坑，必须提醒。
    if let Some(raw) = read_any(&["AIAPI_CAPTCHA_ENABLED", "AGENT2API_CAPTCHA_ENABLED"]) {
        let value = raw.trim();
        if !matches!(value, "0" | "1") {
            warnings.push(format!(
                "AIAPI_CAPTCHA_ENABLED 的取值「{value}」不在预期内：只认 0（关）与 1（开），其它值一律按「开」处理（注意 false/off/no 也会被当成开）"
            ));
        }
    }

    // ── 数据目录 ────────────────────────────────────────────
    if let Some(raw) = read_any(&[
        "AIAPI_PROXY_HOME",
        "AGENT2API_PROXY_HOME",
        "WORKBUDDY_PROXY_HOME",
    ]) {
        let value = raw.trim();
        if value.is_empty() {
            warnings.push("AIAPI_PROXY_HOME 是空串：将回落到默认目录 ~/.aiapi".to_string());
        }
    }

    Ok(warnings)
}

/// 读一组候选环境变量（按顺序取第一个**非空**的；兼容改名前后的旧名）。
fn read_any(names: &[&str]) -> Option<String> {
    for name in names {
        if let Ok(value) = env::var(name) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// 解析端口：必须能转成 1..=65535 的 u16（0 与非法都算错）。
fn parse_port(raw: &str, name: &str) -> Result<u16, String> {
    match raw.trim().parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(format!("{name} 不是合法端口（应为 1-65535 的整数）: {raw}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_port_accepts_valid_and_rejects_others() {
        assert_eq!(parse_port("3065", "P").unwrap(), 3065);
        assert!(parse_port("0", "P").is_err());
        assert!(parse_port("70000", "P").is_err());
        assert!(parse_port("abc", "P").is_err());
        assert!(parse_port("", "P").is_err());
    }
}
