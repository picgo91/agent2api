//! Antigravity 账号：OAuth 凭证（accessToken / refreshToken / expiresAt）+
//! 账号标识（email、projectId）。
//!
//! ── 身份怎么认 ──────────────────────────────────────────────
//! `email`（id_token 里解出来的）是账号身份：同一 Google 账号重复登录就地更新
//! 同一条记录，而不是在列表里堆两条。email 缺失（手动粘贴只给了 token 串）时
//! 依次退回 `refreshToken` 相等 → `accessToken` 相等去重 —— 同一串凭证重复添加
//! 命中同一条记录。
//!
//! ── 记录 id 怎么生成 ────────────────────────────────────────
//! `antigravity-<sha256(种子)前 16 字节>`，种子优先级 email → refreshToken →
//! accessToken：email 稳定（换 token 不换 id），refreshToken 在「只粘了刷新型
//! 凭证」时也稳定；只有 accessToken 可用的账号（无续期能力）id 会随换发变化，
//! 那时本来就是「重新粘贴一份新凭证」，去重只能靠 email/refreshToken。
//!
//! ── 空值为什么不覆盖 ────────────────────────────────────────
//! `Credentials::to_value` 把空串落成 `null`：只粘了 accessToken 的重新添加
//! 不该把能续期的 refreshToken / projectId 洗成空（洗掉等于判了这个账号死刑，
//! 下次续期才发现）。与 Accio / CodeArts 同一套合并规则。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::server::core::providers::antigravity::credentials::Credentials;
use crate::server::core::providers::{kind_id, ProviderKind};
use crate::server::logging;

use super::priority::next_free_priority;
use super::sql;
use super::state::StoredAccount;
use super::store::{AccountStore, AccountStoreError};
use super::store_util::{max_concurrent_public, token_tail_of, truncate_chars};
use super::CredentialWrite;

/// 本家的 provider id（`store_view` 的形状分派与别处比对都要用，
/// 与其它几家同一命名：`<X>_PROVIDER_ID`）。
pub const ANTIGRAVITY_PROVIDER_ID: &str = kind_id(ProviderKind::Antigravity);

const PROVIDER: &str = ANTIGRAVITY_PROVIDER_ID;

/// 刷新前用来比对「还是不是同一份凭证」的键（变了说明期间被重新添加/导入过）。
const IDENTITY_KEYS: &[&str] = &["accessToken", "refreshToken", "email", "addedAt"];

impl AccountStore {
    /// 取一条 Antigravity 账号记录；`account_id` 为空时取「启用且能用」的第一条
    /// （与转发默认选路同一口径）。
    pub fn antigravity_account_record(&self, account_id: &str) -> Option<Value> {
        let guard = self.guard();
        if !account_id.is_empty() {
            let record = self.record_by_id(&guard, account_id)?;
            return (record.provider() == PROVIDER).then(|| record.to_value());
        }
        self.records_for_provider(&guard, PROVIDER)
            .into_iter()
            .filter(|record| record.enabled() && record.has_token())
            .min_by_key(|record| record.order_key())
            .map(|record| record.to_value())
    }

    /// 添加 / 更新一条 Antigravity 账号（手动粘贴、导入与网页登录共用这一个入口）。
    pub fn add_antigravity_account(
        &self,
        credentials: &Credentials,
        name: Option<&str>,
        source: &str,
    ) -> Result<Value, AccountStoreError> {
        if credentials.access_token.is_empty() && credentials.refresh_token.is_empty() {
            return Err(AccountStoreError::bad_request(
                "Antigravity 凭证缺少 accessToken 与 refreshToken",
            ));
        }
        let guard = self.guard();
        // 身份命中既有记录就地更新（重复登录/重复导入不该在列表里堆两条）
        let existing = self
            .records_for_provider(&guard, PROVIDER)
            .into_iter()
            .find(|record| same_identity(record, credentials));
        let id = existing
            .as_ref()
            .map(|record| record.id().to_string())
            .unwrap_or_else(|| {
                let seed = if !credentials.email.is_empty() {
                    credentials.email.clone()
                } else if !credentials.refresh_token.is_empty() {
                    credentials.refresh_token.clone()
                } else {
                    credentials.access_token.clone()
                };
                format!("antigravity-{:x}", Sha256::digest(seed.as_bytes()))
            });
        if existing.is_none() && self.record_by_id(&guard, &id).is_some() {
            return Err(AccountStoreError::new(
                "Antigravity 账号 ID 已被其它账号占用，请先核对账号记录",
                409,
            ));
        }
        let record_name = name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                existing
                    .as_ref()
                    .map(StoredAccount::name)
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| {
                if !credentials.name.is_empty() {
                    Some(credentials.name.clone())
                } else if !credentials.email.is_empty() {
                    Some(credentials.email.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "Antigravity 账号".to_string());
        let mut fields = existing
            .as_ref()
            .map(|record| record.fields().clone())
            .unwrap_or_default();
        if let Value::Object(values) = credentials.to_value() {
            for (key, value) in values {
                // 空值不覆盖既有内容（见模块头「空值为什么不覆盖」）
                if value.is_null() || matches!(&value, Value::String(text) if text.is_empty()) {
                    continue;
                }
                fields.insert(key, value);
            }
        }
        let priority = match existing.as_ref() {
            Some(record) => record.priority(),
            None => {
                let used = self
                    .with_conn(&guard, |conn| sql::priorities_all(conn))
                    .unwrap_or_default();
                next_free_priority(&used)
            }
        };
        fields.insert("id".to_string(), Value::String(id.clone()));
        fields.insert("provider".to_string(), Value::String(PROVIDER.to_string()));
        fields.insert(
            "name".to_string(),
            Value::String(truncate_chars(&record_name, 100)),
        );
        fields.insert(
            "tokenTail".to_string(),
            Value::String(token_tail_of(&display_token(credentials))),
        );
        fields.insert("priority".to_string(), Value::from(priority));
        fields.insert(
            "enabled".to_string(),
            Value::Bool(
                existing
                    .as_ref()
                    .map(StoredAccount::enabled)
                    .unwrap_or(true),
            ),
        );
        fields.insert("desktop".to_string(), Value::Bool(false));
        fields.insert("source".to_string(), Value::String(source.to_string()));
        fields.insert(
            "addedAt".to_string(),
            Value::from(
                existing
                    .as_ref()
                    .map(StoredAccount::added_at)
                    .unwrap_or_else(logging::now_ms),
            ),
        );
        fields.insert("updatedAt".to_string(), Value::from(logging::now_ms()));
        fields.insert("rateLimits".to_string(), json!({}));
        let record = StoredAccount::from_map(fields);
        self.with_conn(&guard, |conn| sql::put(conn, &record))?;
        logging::log(
            "[Accounts]",
            &format!("✅ Antigravity 账号已保存（优先级 {priority}）"),
        );
        Ok(self.public_account(&record))
    }

    /// 刷新结果回写（比较-再写：记录里的凭证仍是刷新前那份才写）。
    ///
    /// 单飞只保证同进程内不并发；**重新添加/导入**会在这期间换掉记录，
    /// 所以写回前要比对身份键（比对失败返回 `Stale`，调用方重读而不是覆盖）。
    pub fn update_antigravity_credentials_if_current(
        &self,
        expected: &Value,
        credentials: &Credentials,
    ) -> Result<CredentialWrite, AccountStoreError> {
        let id = expected.get("id").and_then(Value::as_str).unwrap_or("");
        let guard = self.guard();
        let Some(mut record) = self
            .record_by_id(&guard, id)
            .filter(|record| record.provider() == PROVIDER)
        else {
            return Ok(CredentialWrite::Stale);
        };
        for key in IDENTITY_KEYS {
            if record.get(key) != expected.get(key) {
                return Ok(CredentialWrite::Stale);
            }
        }
        // 身份必须对得上：email 不一致说明刷新结果属于另一个账号（绝不落盘串号）
        let stored_email = record
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if !credentials.email.is_empty()
            && !stored_email.is_empty()
            && stored_email != credentials.email
        {
            return Err(AccountStoreError::bad_request(
                "Antigravity 刷新结果与原账号身份不一致",
            ));
        }
        if let Value::Object(values) = credentials.to_value() {
            for (key, value) in values {
                // 空值不覆盖：续期响应没有 email/projectId，不该把它们洗掉；
                // refreshToken 由调用方（refresh.rs）保证「上游没给新值时保留旧值」
                if value.is_null() || matches!(&value, Value::String(text) if text.is_empty()) {
                    continue;
                }
                record.fields_mut().insert(key, value);
            }
        }
        record.set(
            "tokenTail",
            Value::String(token_tail_of(&display_token(credentials))),
        );
        record.set_updated_at(logging::now_ms());
        self.with_conn(&guard, |conn| sql::update_in_place(conn, &record))?;
        Ok(CredentialWrite::Written)
    }

    /// 界面用的公开形态（**绝不带任何凭证字段**）。
    pub fn to_antigravity_public_account(&self, record: &StoredAccount) -> Value {
        let stored = record.to_value();
        let credentials = Credentials::from_payload(&stored).ok();
        let available = record.has_token() && credentials.is_some();
        let can_refresh = credentials.as_ref().is_some_and(Credentials::can_refresh);
        let mut public = serde_json::Map::new();
        for key in [
            "id",
            "provider",
            "name",
            "email",
            "projectId",
            "source",
            "tokenTail",
            "expiresAt",
        ] {
            public.insert(
                key.to_string(),
                record.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        public.insert("edition".to_string(), Value::Null);
        public.insert("editionLabel".to_string(), Value::Null);
        public.insert("hasRefreshToken".to_string(), Value::Bool(can_refresh));
        public.insert("priority".to_string(), Value::from(record.priority()));
        public.insert("enabled".to_string(), Value::Bool(record.enabled()));
        public.insert("addedAt".to_string(), Value::from(record.added_at()));
        public.insert("updatedAt".to_string(), Value::from(record.updated_at()));
        public.insert(
            "proxy".to_string(),
            crate::server::core::proxies::describe_account_proxy(Some(&record.proxy())),
        );
        public.insert(
            "rateLimits".to_string(),
            record
                .get("rateLimits")
                .cloned()
                .unwrap_or_else(|| json!({})),
        );
        public.insert("desktop".to_string(), Value::Bool(false));
        public.insert("available".to_string(), Value::Bool(available));
        public.insert(
            "maxConcurrent".to_string(),
            Value::from(max_concurrent_public(record.get("maxConcurrent"))),
        );
        Value::Object(public)
    }
}

/// 记录与本家凭证是否同一身份（email → refreshToken → accessToken）。
fn same_identity(record: &StoredAccount, credentials: &Credentials) -> bool {
    let field = |key: &str| {
        record
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let email = field("email");
    if !credentials.email.is_empty() && !email.is_empty() {
        return email == credentials.email;
    }
    let refresh = field("refreshToken");
    if !credentials.refresh_token.is_empty() && !refresh.is_empty() {
        return refresh == credentials.refresh_token;
    }
    let access = field("accessToken");
    !credentials.access_token.is_empty() && !access.is_empty() && access == credentials.access_token
}

/// 尾号取自显示的 token：优先 accessToken，只有刷新型凭证时取 refreshToken
/// （空尾号会让界面显示成「没有凭证」，与事实不符）。
fn display_token(credentials: &Credentials) -> String {
    if !credentials.access_token.is_empty() {
        credentials.access_token.clone()
    } else {
        credentials.refresh_token.clone()
    }
}

/// 记录是否属于本家（store_view 的形状分派用）。
pub fn is_antigravity_record(record: &StoredAccount) -> bool {
    record.provider() == PROVIDER
}
