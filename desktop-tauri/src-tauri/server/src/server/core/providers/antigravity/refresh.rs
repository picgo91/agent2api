//! Antigravity 凭证续期：同一凭证只刷一次，保存时确认账号未被重新导入或删除。
//!
//! 上游：`POST https://oauth2.googleapis.com/token`（form），
//! `grant_type=refresh_token` → `{access_token, expires_in}`（spec §A.4）。
//!
//! ── 上游不回新 refresh_token 时保留旧值 ─────────────────────
//! Google 只在首次授权（`prompt=consent`）下发一次 refresh_token；续期响应里
//! 通常没有它。这条实现**从不**覆盖非空的 refresh_token，因此刷完还是同一套
//! 续期能力（覆盖成空串等于把账号刷死）。
//!
//! ── 上游没回过期时间怎么办 ──────────────────────────────────
//! `expires_in` 缺失时给一个保守的默认值（1 小时，与 Accio 同取舍）：它只影响
//! 「下次要不要续期」的判定，给短了会多刷一次（无害），给长了会让失效 token
//! 留着不走续期路径（有害）。

use std::sync::OnceLock;

use serde_json::Value;

use crate::server::core::account_store::{AccountStore, CredentialWrite};
use crate::server::core::providers::refresh_flight::{self, Join, Table};
use crate::server::errors::GatewayError;
use crate::server::logging;

use super::auth;
use super::credentials::Credentials;
use super::oauth;

/// 上游没给到期时间时的保守默认：1 小时
const DEFAULT_TTL_MS: i64 = 60 * 60 * 1000;

static FLIGHTS: OnceLock<Table<Credentials>> = OnceLock::new();

/// 取账号记录 + 凭证（不刷新）
pub fn snapshot(store: &AccountStore, account_id: &str) -> Result<(Value, Credentials), GatewayError> {
    let record = store
        .antigravity_account_record(account_id)
        .ok_or_else(|| GatewayError::with_status(404, "Antigravity 账号不存在，请先添加账号"))?;
    let credentials = auth::snapshot(&record)?;
    Ok((record, credentials))
}

/// 确保拿到可用凭证（`force = true` 时不看临期窗口，401 之后强制刷一次）。
pub async fn ensure_fresh(
    store: &AccountStore,
    account_id: &str,
    force: bool,
) -> Result<Credentials, GatewayError> {
    let (record, credentials) = snapshot(store, account_id)?;
    if !force && !credentials.expiring() {
        return Ok(credentials);
    }
    if !credentials.can_refresh() {
        return Err(GatewayError::with_status(
            400,
            "Antigravity 账号没有 refreshToken，无法续期，请重新登录或粘贴完整凭证",
        ));
    }
    let key = format!(
        "{}:{}:{}:{}",
        store.file_string(),
        account_id,
        refresh_flight::fingerprint(&credentials.access_token),
        refresh_flight::fingerprint(&credentials.refresh_token),
    );
    match FLIGHTS.get_or_init(Table::new).join(&key) {
        Join::Waiter(waiter) => waiter.wait().await,
        Join::Leader(leader) => {
            let result = refresh_and_save(store, &record, &credentials).await;
            leader.finish(result.clone());
            result
        }
    }
}

async fn refresh_and_save(
    store: &AccountStore,
    record: &Value,
    credentials: &Credentials,
) -> Result<Credentials, GatewayError> {
    let proxy = auth::account_proxy(record)?;
    let refreshed = oauth::refresh_access_token(&credentials.refresh_token, proxy.as_ref()).await?;
    let mut fresh = credentials.clone();
    fresh.access_token = refreshed.access_token;
    fresh.expires_at = Some(
        refreshed
            .expires_at
            .unwrap_or_else(|| logging::now_ms() + DEFAULT_TTL_MS),
    );
    // refresh_token 保留旧值（上游一般不给新的；覆盖成空等于刷死账号）
    // email/project 保留旧值（续期响应没有它们，`to_value` 的空值不覆盖规则兜住）
    match store
        .update_antigravity_credentials_if_current(record, &fresh)
        .map_err(|error| GatewayError::with_status(error.status_code, error.message))?
    {
        CredentialWrite::Written => Ok(fresh),
        CredentialWrite::Stale => {
            // 回写被拒（记录已被换掉）：用最新那份，别把旧结果当成功用出去
            let id = record.get("id").and_then(Value::as_str).unwrap_or("");
            snapshot(store, id).map(|(_, credentials)| credentials)
        }
    }
}
