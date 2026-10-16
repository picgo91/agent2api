//! Antigravity 适配器：账号管理 **+ 推理转发**（Google cloudcode-pa /
//! `v1internal` 官方信封）。
//!
//! ── 上游长什么样（逆向来源：spec；CLIProxyAPI 与 Antigravity-Manager
//!    两份参考实现逐行核对）────────────────────────────────────
//!   - 登录：Google OAuth 2.0 授权码（固定回调 `http://localhost:51121/oauth-callback`，
//!     无 PKCE），见 `oauth.rs`；
//!   - 推理：`POST {base}/v1internal:streamGenerateContent?alt=sse`，body 是
//!     `{project, requestId, request, model, userAgent, requestType}` 信封，
//!     鉴权走标准 `Authorization: Bearer`（见 `protocol.rs`）；
//!   - 续期：`oauth2.googleapis.com/token` 的 `refresh_token` 流（见 `refresh.rs`）；
//!   - **没有签到**：上游没有那个活动，`billing::checkin` 的范围判定把这家排除；
//!   - **没有余额接口**：`supports_usage()` 为 false（上游配额接口
//!     `retrieveUserQuotaSummary` 的形态未在 spec 中固化，先不接）。
//!
//! ── 为什么走会话式转发（`is_stateful`）────────────────────────
//! 与 Accio 同理：请求体要转成官方信封、响应是 `{"response": …}` 包裹的 SSE，
//! 无状态路径（原样发、原样收）两条都做不到。实现收进 `chat.rs`，产出仍是
//! `ForwardOutcome`，网关其余链路零改动。
//!
//! ── 静态目录（`supports_model_refresh() = false`）──────────────
//! 官方客户端把模型目录打包在资源里（`resources/official_models.json`），
//! 34 条静态表内联在 `models.rs`（CLIProxyAPI 同取舍）。用户手动点「刷新模型」
//! 对这家是空操作（`ModelRefreshOutcome::unchanged`），界面据
//! `supports_model_refresh` 如实说明。
//!
//! ── 未验证项（接上线后第一次真实调用要盯的）──────────────────
//!   1. `loadCodeAssist` / `onboardUser` 对普通消费者账号的完整行为
//!      （spec 只覆盖了两个参考实现的调用形态，免费档的项目创建时机未知）；
//!   2. 信封 `project` 兜底值（`bamboo-precept-lgxtn`）是否被上游接受；
//!   3. Claude 系对 `thinkingLevel` 的接受范围（我们按目录声明的档位收敛，
//!      但 `max` 档是否真被上游认，spec 未给实测）。
//!
//! ── panic=abort ────────────────────────────────────────────
//! 本模块在对话链路上，绝不 unwrap/expect/panic。

pub mod auth;
pub mod chat;
pub mod credentials;
pub mod endpoints;
pub mod models;
pub mod oauth;
pub mod project;
pub mod protocol;
pub mod refresh;
pub mod stream;

use axum::http::HeaderMap;
use serde_json::{json, Value};

use crate::server::core::account_store::AccountStore;
use crate::server::core::providers::adapter::{
    adapter_for, ChatRequestPlan, ModelRefreshOutcome, ProviderAdapter, ReasoningPatch,
    UpstreamErrorClass,
};
use crate::server::core::providers::content_block;
use crate::server::core::providers::ProviderKind;
use crate::server::core::proxies::{resolve_account_proxy, ProxyResolution, ResolvedProxy};
use crate::server::core::upstream::usage::RequestTelemetry;
use crate::server::core::upstream::ForwardOutcome;
use crate::server::errors::GatewayError;
use crate::server::logging;

use self::credentials::Credentials;

/// 适配器实例（`adapter_for(ProviderKind::Antigravity)` 给出这一个）
pub static ANTIGRAVITY_ADAPTER: AntigravityAdapter = AntigravityAdapter;

/// Antigravity 适配器。**无状态**：清单是进程级静态表（`models.rs`），
/// 登录状态由登录任务表管理（`oauth.rs` 不做 pending 表）。
pub struct AntigravityAdapter;

/// 思考档位注入的字段名（客户端语义键；`protocol.rs` 在构造信封时消费它）
const REASONING_FIELD: &str = "reasoning_effort";

impl ProviderAdapter for AntigravityAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Antigravity
    }

    /// Antigravity 参与推理转发
    fn supports_chat(&self) -> bool {
        true
    }

    /// 请求体要转成官方信封、响应要拆 `{"response": …}` 帧（见模块头）
    fn is_stateful(&self) -> bool {
        true
    }

    fn list_models(&self) -> Vec<Value> {
        models::list()
    }

    /// 防御性报错：Antigravity 的请求体要先转信封，通用层的序列化路径产不出
    /// 可用的字节。走到这里说明编排层的 `is_stateful` 分流坏了。
    fn build_chat_request(
        &self,
        _account: &Value,
        _body: &Value,
        _client_headers: &HeaderMap,
    ) -> Result<ChatRequestPlan, GatewayError> {
        Err(GatewayError::with_status(
            503,
            "Antigravity 走会话式转发，不走单请求路径（内部错误：编排层未按 is_stateful 分流）",
        ))
    }

    /// 非 2xx 的分类。**注意**额度/鉴权问题也可能出现在 200 的 SSE 错误帧里
    /// （`{"error": {…}}`），那些由 `forward_conversation` 直接转成带状态码的
    /// 网关错误。
    fn classify_error(&self, status: u16, error_body: &Value) -> UpstreamErrorClass {
        let raw = error_body
            .get("message")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .or_else(|| {
                error_body
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
            })
            .unwrap_or("上游错误");
        let message = format!("上游返回 {status}: {raw}");
        match protocol::classify_upstream_error(status, raw) {
            protocol::UpstreamKind::Auth => UpstreamErrorClass::TokenExpired { message },
            protocol::UpstreamKind::Quota => UpstreamErrorClass::QuotaLimited {
                reset_at: None,
                message,
                upstream_code: error_body.get("code").and_then(Value::as_i64),
                status: if status == 0 { 429 } else { status },
            },
            // 内容策略拦截 → ContentBlocked（不罚账号，换中性提示词重试一次）
            protocol::UpstreamKind::Other => content_block::classify_or_fatal(
                status,
                error_body,
                message,
                error_body.get("code").and_then(Value::as_i64),
            ),
        }
    }

    fn ensure_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            refresh::ensure_fresh(store, account_id, false)
                .await
                .map(|credentials| credentials.access_token)
        })
    }

    /// 401 之后的**强制**续期：不看临期窗口（token 可能时间上还新但已被服务端
    /// 失效 —— 只调 ensure 会拿回同一个被拒的 token）。
    fn refresh_access_token<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            refresh::ensure_fresh(store, account_id, true)
                .await
                .map(|credentials| credentials.access_token)
        })
    }

    fn supports_refresh(&self) -> bool {
        true
    }

    fn credentials_expiring(&self, store: &AccountStore, account_id: &str) -> bool {
        store
            .antigravity_account_record(account_id)
            .and_then(|record| Credentials::from_payload(&record).ok())
            .is_some_and(|credentials| credentials.expiring())
    }

    /// Antigravity **没有**「默认模型」概念：不指定模型时由它自己的目录决定
    fn supports_default_model(&self) -> bool {
        false
    }

    /// 思考等级绑定 → `reasoning_effort`（值原样交给协议层按模型声明集合收敛）。
    ///
    /// 客户端已显式指定时不覆盖；档位不在通用 6 档表里时不注入（上游对新词
    /// 要么忽略要么 400，都不如明确跳过并说清原因）。`off` / `none` 在注入点
    /// 就被拦下 —— Antigravity 没有可靠的「关闭思考」表达（`thinkingLevel`
    /// 没有 off 档，`includeThoughts: false` 对 Claude 系的行为未知）。
    fn reasoning_patch(&self, level: &str, _model: &str, body: &Value) -> ReasoningPatch {
        if crate::server::core::model_rules::reasoning_is_off(level) {
            return ReasoningPatch::Skip {
                reason: "Antigravity 没有「关闭思考」的可靠表达，跳过注入",
            };
        }
        if crate::server::core::model_rules::read_client_level(body).is_some() {
            return ReasoningPatch::Skip {
                reason: "客户端请求体里已指定思考等级，绑定让位",
            };
        }
        if crate::server::core::model_rules::reasoning_rank(level).is_none() {
            return ReasoningPatch::Skip {
                reason: "该等级不在通用档位表内，Antigravity 不注入未知档位",
            };
        }
        ReasoningPatch::Set {
            field: REASONING_FIELD,
            value: Value::String(level.to_string()),
        }
    }

    /// 静态目录（见模块头）：没有可拉取的远程清单
    fn supports_model_refresh(&self) -> bool {
        false
    }

    /// 静态目录的刷新是**空操作**（`implemented_kinds` 仍列入本家：它回答的是
    /// 「接线了没有」，不是「目录能不能远程刷」——与 ZCode 同口径）。
    fn refresh_models<'a>(
        &'a self,
        _store: &'a AccountStore,
        _account_id: &'a str,
        _force: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ModelRefreshOutcome> + Send + 'a>> {
        Box::pin(async move { ModelRefreshOutcome::unchanged() })
    }

    /// Antigravity 支持网页登录（Google OAuth；回调由壳 / Web 面板送回网关，
    /// 见 `oauth.rs` 的模块头）
    fn supports_web_login(&self) -> bool {
        true
    }

    /// 授权地址：Google 授权页，`state = antigravity-<random>`
    /// （**固定回调** `http://localhost:51121/oauth-callback`，不含本机端口）。
    fn build_login_url(&self) -> Option<(String, String)> {
        Some(oauth::begin_login())
    }

    /// 用回调里的一次性 `code` 换凭证并落账号。
    ///
    /// `proxy` 是界面在添加弹窗里可选填的出口代理（账号形态 JSON）：Google
    /// 的 token 端点在本机直连不通时（中国大陆网络下 `oauth2.googleapis.com`
    /// 必然超时），换码会走它；成功后同一个配置写进新账号 —— 否则令牌刷新与
    /// 推理转发的出口仍然是直连，「登录成功但一用就超时」。
    fn exchange_login_code<'a>(
        &'a self,
        store: &'a AccountStore,
        code: &'a str,
        state: &'a str,
        proxy: Option<&'a Value>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            // 深度防御：调用链上游已按前缀分派过，这里再核对一次
            if !state.starts_with(oauth::STATE_PREFIX) {
                return Err(GatewayError::with_status(
                    400,
                    "这次登录不属于 Antigravity（state 前缀不匹配），请重新发起",
                ));
            }
            // 代理解析失败必须在换码前报错：静默直连会让用户看到的是
            // 「连接 Google 超时」，而真正的原因（填的代理引用无效 /
            // Clash 出口没在跑）被藏起来，是最难查的一类问题
            let resolved = match proxy {
                None => None,
                Some(config) => match resolve_account_proxy(Some(config)) {
                    Some(ProxyResolution::Resolved(proxy)) => Some(proxy),
                    Some(ProxyResolution::Failed(message)) => {
                        return Err(GatewayError::with_status(
                            400,
                            format!("登录代理不可用：{message}"),
                        ));
                    }
                    None => None,
                },
            };
            let credentials = oauth::exchange_code(code, resolved.as_ref()).await?;
            let account = store
                .add_antigravity_account(&credentials, None, "web")
                .map_err(|error| GatewayError::with_status(error.status_code, error.message))?;
            let account_id = account
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // 换码用的出口写进新账号（与账号页「代理」同一形状、同一校验）。
            // 写失败不推翻这次登录：账号已经落库，用户在账号页补一次即可 ——
            // 为一次可修的回写失败丢掉刚换来的凭证才是更坏的结果。
            if let Some(config) = proxy.filter(|_| !account_id.is_empty()) {
                if let Err(error) = store.update_account(&account_id, &json!({ "proxy": config })) {
                    logging::log(
                        "[Login]",
                        &format!(
                            "⚠️ Antigravity 登录代理未能写入账号 {}：{}（可在账号页手动补）",
                            account_id, error.message
                        ),
                    );
                }
            }
            logging::log(
                "[Login]",
                &format!(
                    "✅ Antigravity 网页登录完成，账号已加入列表（{}）",
                    auth::describe(&credentials)
                ),
            );
            Ok(account_id)
        })
    }

    /// **会话式转发入口**：构造 → 发送 → 翻译。
    fn forward_conversation<'a>(
        &'a self,
        store: &'a AccountStore,
        account_id: &'a str,
        body: &'a Value,
        _client_headers: &'a HeaderMap,
        proxy: Option<ResolvedProxy>,
        stream: bool,
        telemetry: &'a std::sync::Arc<RequestTelemetry>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ForwardOutcome, GatewayError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let account_id = account_id.to_string();
            if account_id.is_empty() {
                return Err(GatewayError::with_status(
                    503,
                    "没有可用的 Antigravity 账号：请在账号页添加并启用账号",
                ));
            }
            let model_name = body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // 凭证（含临期主动刷新）。显式账号必填 —— Antigravity 没有环境变量旁路。
            let context = account_context(store, &account_id).await?;
            // 思考档位：客户端显式指定 > 映射绑定（`body` 里已由 payload 层注入）
            let effort = protocol::client_effort(body);
            let plan =
                chat::build_plan(&context.credentials, body, &model_name, effort.as_deref())?;
            let effective_proxy = proxy.or(context.proxy);

            logging::verbose(
                "[Antigravity]",
                &format!(
                    "POST {} model={} upstream={} stream={} account={} project={}",
                    plan.url,
                    if model_name.is_empty() {
                        "(默认)"
                    } else {
                        &model_name
                    },
                    plan.upstream_key,
                    stream,
                    account_id,
                    if context.credentials.project_id.is_empty() {
                        "(兜底)"
                    } else {
                        &context.credentials.project_id
                    },
                ),
            );

            let capture = telemetry.capture();
            if let Some(capture) = capture.as_deref() {
                let headers: Vec<(String, String)> = plan.headers.clone();
                capture.reset_request(&plan.url, "antigravity", &headers, body);
            }

            let response = chat::send(&plan, effective_proxy.as_ref()).await?;
            if let Some(capture) = capture.as_deref() {
                capture.attach_response(response.status().as_u16(), response.headers());
            }
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let error = chat::http_error(status, response).await;
                if error.status_code == 429 {
                    chat::record_limited(
                        &LimitContext {
                            store: store.clone(),
                            account_id: account_id.clone(),
                            model: model_name.clone(),
                        },
                        status,
                        &error.message,
                    );
                }
                return Err(error);
            }

            let translator = chat::Translator::new(chat::response_id(), plan.model_name.clone());
            let limit = chat::LimitContext {
                store: store.clone(),
                account_id: account_id.clone(),
                model: model_name.clone(),
            };
            if stream {
                // 首帧预读：额度/鉴权错误在 200 + 错误帧里，拦在返回之前换号无损
                let (prefetched, source) = chat::prefetch_stream_head(response, &limit).await?;
                let (sender, receiver) =
                    tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(64);
                let telemetry = telemetry.clone();
                crate::spawn_task(async move {
                    chat::drive_stream(source, translator, telemetry, limit, sender, prefetched)
                        .await;
                });
                return Ok(ForwardOutcome::Stream {
                    status: 200,
                    stream: Box::new(tokio_stream::wrappers::ReceiverStream::new(receiver)),
                });
            }
            // 非流式：上游也只走流式端点（见 chat.rs 模块头），聚合后返回
            match chat::drive_aggregate(response, translator, telemetry.clone(), &limit).await {
                Ok(body) => Ok(ForwardOutcome::Completion { body }),
                Err(error) => Err(error),
            }
        })
    }
}

/// 一次转发用的凭证 + 出口快照
struct AccountContext {
    credentials: Credentials,
    proxy: Option<ResolvedProxy>,
}

/// 取账号凭证（含临期主动刷新）与出口。
async fn account_context(
    store: &AccountStore,
    account_id: &str,
) -> Result<AccountContext, GatewayError> {
    let credentials = refresh::ensure_fresh(store, account_id, false).await?;
    let proxy = store
        .antigravity_account_record(account_id)
        .map(|record| auth::account_proxy(&record))
        .transpose()?
        .flatten();
    Ok(AccountContext { credentials, proxy })
}

/// 会话式转发里的限额记账素材（与 Accio 同名类型同分工）
pub use chat::LimitContext;

/// 适配器实例自检（`implemented_kinds` 与 `adapter_for` 的一致性由编译期保证，
/// 这里只给一处便于排障的名字）
pub fn adapter_label() -> &'static str {
    "Antigravity"
}

/// 显式引用 `adapter_for`：让「加 kind 忘了接线」在本文件也会被编译器提醒
/// （模块头的约定，编译期断言的一种轻量形态）
pub fn adapter_for_self() -> &'static dyn ProviderAdapter {
    adapter_for(ProviderKind::Antigravity)
}
