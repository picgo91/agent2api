//! Antigravity 的网页登录任务（Google OAuth 授权码，固定回调
//! `http://localhost:51121/oauth-callback`）。
//!
//! ── 这条链在几家登录里排第几种形态 ──────────────────────────
//! ```text
//!   workbuddy  上游推 authUrl（auth/state）→ 轮询 auth/token
//!   小浣熊      本地拼静态授权地址 → 等**壳侧窗口**捕获的自定义协议回调
//!   Qoder      同步问上游要设备码 → 轮询设备令牌
//!   CatPaw     同步问上游要登录入口 → 等**上游 POST 到本机**的回调
//!   Cline      同步问上游要设备码 → 轮询
//!   AutoClaw   前端过一次风控验证码 → 网关拿授权地址 → 等**浏览器 302 到本机**
//!   Accio      本地拼授权地址（PKCE）→ 等**浏览器 302 到本机**
//!   CodeArts   portal 两跳回调 → ticket 换正式凭证
//!   Antigravity 本地拼授权地址（无 PKCE，回调固定 51121 端口）→
//!               由**壳/面板把回调转交到网关** `/oauth/callback`
//! ```
//!
//! ── 为什么回调不经 `finish_codearts_login` 而在这里收尾 ──────
//! Google 的固定回调指向本机 `localhost:51121`，`antigravity` 的 CLI 自己会
//! 监听那个端口；网关**不抢占**它。桌面壳在两处回调拦截点（见
//! `desktop-tauri/src-tauri/src/login.rs`）把 `localhost:51121/oauth-callback`
//! 重写成网关的 `/oauth/callback` 再转发，Web 面板则让用户把地址栏里的主机端口
//! 换成网关地址 —— 两条路的终点都是 `/oauth/callback` 那条既有路由。
//! 该路由先按 `state` 前缀（`antigravity-`）分派到这里，其余 state 才走
//! CodeArts 的收尾。**没有 pending 表**：任务表本身就是事实来源
//! （`state` 由 `LoginService::start_web_login` 的任务持有，回调按 state 查任务）。
//!
//! ── 校验口径 ────────────────────────────────────────────────
//!   1. 查询串里的 state 必须对应一个**进行中且属于 Antigravity** 的登录任务；
//!   2. 授权码一次性：任务已完成时直接返回成功（浏览器刷新/预取会重复回调，
//!      第二次必然撞上「授权码已失效」，把一次成功变成一次失败）；
//!   3. `error=access_denied`（用户点了拒绝）走任务失败收尾，不是 500。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::server::core::account_store::antigravity_accounts::ANTIGRAVITY_PROVIDER_ID;
use crate::server::core::providers::adapter::adapter_for;
use crate::server::core::providers::antigravity::oauth;
use crate::server::core::providers::ProviderKind;
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::{finish_task_error, LoginService};

impl LoginService {
    /// 浏览器回调：按 state 找任务 → 用授权码换凭证 → 落账号 → 标记任务完成。
    ///
    /// `params` 是回调的查询串（`code` / `state` / 可选的 `error`）。
    pub async fn finish_antigravity_login(
        &self,
        params: &HashMap<String, String>,
    ) -> Result<String, GatewayError> {
        let state = params
            .get("state")
            .map(|value| value.trim())
            .unwrap_or("");
        if state.is_empty() {
            return Err(GatewayError::with_status(
                400,
                "回调没有携带 state，无法确认这次登录归属",
            ));
        }
        let Some(handle) = self.tasks.get(state) else {
            return Err(GatewayError::with_status(
                404,
                "这次登录已取消或已过期，请重新发起",
            ));
        };
        let snapshot = handle.snapshot();
        if snapshot.provider != ANTIGRAVITY_PROVIDER_ID {
            return Err(GatewayError::with_status(
                400,
                "这次登录不属于 Antigravity",
            ));
        }
        if snapshot.done {
            // 幂等：重复回调不是错误（见校验口径 2）
            return Ok(String::new());
        }
        // error 参数（用户拒绝 / 上游报错）与缺 code 都在这里变成 400 文案，
        // 同时把任务标成失败 —— 否则面板会一直转圈到任务超时。
        let code = match oauth::validate_callback(params) {
            Ok(code) => code,
            Err(error) => {
                finish_task_error(&handle, &error.message);
                return Err(error);
            }
        };
        match adapter_for(ProviderKind::Antigravity)
            .exchange_login_code(&self.store, &code, state)
            .await
        {
            Ok(account_id) => {
                handle.update(|task| {
                    task.done = true;
                    task.session = Some(json!({
                        "accountUid": account_id,
                        "nickname": Value::Null,
                        "provider": ANTIGRAVITY_PROVIDER_ID,
                    }));
                    task.finished_at = Some(logging::now_ms());
                });
                Ok(account_id)
            }
            Err(error) => {
                finish_task_error(&handle, &error.message);
                logging::log(
                    "[Login]",
                    &format!("❌ Antigravity 网页登录换取凭证失败: {}", error.message),
                );
                Err(error)
            }
        }
    }
}
