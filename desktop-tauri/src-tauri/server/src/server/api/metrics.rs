//! GET /metrics —— Prometheus 文本格式的运行指标。
//!
//! ── 为什么是它 ────────────────────────────────────────────────
//! 面板里的「报表」页是给人看的；机器采集（Prometheus / Grafana / Uptime Kuma
//! 之类）需要一个稳定、无鉴权、低开销的端点。数据全部来自已有的
//! `RequestStats`（SQLite 聚合表）与内存里的模型目录，**不新增任何存储**，
//! 也不在采集时触发上游请求。
//!
//! ── 放在哪个路由面 ────────────────────────────────────────────
//! 与 `/health` 一样挂在免鉴权的 open 面上（见 http.rs 的 gateway_router）：
//! 采集器通常拿不到网关 Key，且这些数字是部署者自己机器的聚合量。
//! 但**暴露面按安全形态收敛**：headless 的闸门 / fail-closed 形态下只回
//! 最基础的进程级指标（up / 请求总数），不带按提供商拆分的标签 —— 与
//! `/health` 在同样形态下只回 `{"status":"ok"}` 同一取舍（见 health.rs）。
//!
//! ── 与报表的口径关系 ──────────────────────────────────────────
//! `aiapi_requests_total` 等取自 `usage_summary("all")` 的 overview —— 与
//! 报表页「全部」档读的是**同一次聚合**，所以两边数字必然对得上（这正是
//! 复用 `usage_summary` 而不是另写一套 COUNT 的原因）。

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::server::ServerState;

/// 处理 GET /metrics。
pub async fn handle(State(state): State<ServerState>) -> Response {
    let secure = crate::server::access::panel_gate() || crate::server::access::v1_fail_closed();

    let summary = state.request_stats().usage_summary("all");
    let overview = summary.get("overview").cloned().unwrap_or(Value::Null);
    let requests = number_at(&overview, "requests");
    let successful = number_at(&overview, "successful");
    let tokens = number_at(&overview, "tokens");

    let healthy = state
        .auth()
        .get_config_summary()
        .get("configured")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut out = String::with_capacity(1024);
    // up / 进程级
    metric_gauge(
        &mut out,
        "aiapi_up",
        "网关是否已配置可用上游（1=是, 0=否）",
        if healthy { 1 } else { 0 },
    );
    metric_counter(
        &mut out,
        "aiapi_requests_total",
        "已记录的请求总数",
        requests,
    );
    metric_counter(
        &mut out,
        "aiapi_requests_successful_total",
        "其中成功（2xx 且无错误）的请求数",
        successful,
    );
    metric_counter(
        &mut out,
        "aiapi_tokens_total",
        "累计 token 用量（输入+输出）",
        tokens,
    );
    metric_gauge(
        &mut out,
        "aiapi_models",
        "当前模型目录条数",
        state.models().count() as i64,
    );

    // 按提供商拆分：安全形态下不给（会暴露用了哪几家），见模块头说明
    if !secure {
        if let Some(rows) = summary.get("providers").and_then(Value::as_array) {
            out.push_str("# HELP aiapi_provider_requests_total 按提供商的请求数\n");
            out.push_str("# TYPE aiapi_provider_requests_total counter\n");
            for row in rows {
                let id = row.get("id").and_then(Value::as_str).unwrap_or("");
                if id.is_empty() {
                    continue;
                }
                // Prometheus 标签值里反斜杠、双引号、换行必须转义
                let label = escape_label(id);
                out.push_str(&format!(
                    "aiapi_provider_requests_total{{provider=\"{label}\"}} {}\n",
                    number_at(row, "requests")
                ));
            }
            out.push_str("# HELP aiapi_provider_failures_total 按提供商的失败请求数\n");
            out.push_str("# TYPE aiapi_provider_failures_total counter\n");
            for row in rows {
                let id = row.get("id").and_then(Value::as_str).unwrap_or("");
                if id.is_empty() {
                    continue;
                }
                let label = escape_label(id);
                out.push_str(&format!(
                    "aiapi_provider_failures_total{{provider=\"{label}\"}} {}\n",
                    number_at(row, "failures")
                ));
            }
        }
    }

    (
        [(
            header::CONTENT_TYPE,
            // Prometheus 期望的官方 content type；版本号让解析器选对文本格式
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out,
    )
        .into_response()
}

/// 从 JSON 对象里读一个数（缺失 / 类型不对一律当 0）。
fn number_at(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// 写一条 gauge（`# HELP` + `# TYPE` + 一行样本）。
fn metric_gauge(out: &mut String, name: &str, help: &str, value: i64) {
    out.push_str(&format!(
        "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
    ));
}

/// 写一条 counter。
fn metric_counter(out: &mut String, name: &str, help: &str, value: i64) {
    out.push_str(&format!(
        "# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}\n"
    ));
}

/// 转义 Prometheus 标签值（`\` → `\\`、`"` → `\"`、换行 → `\n`）。
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn number_at_reads_or_defaults() {
        let v = json!({ "requests": 42, "bad": "x" });
        assert_eq!(number_at(&v, "requests"), 42);
        assert_eq!(number_at(&v, "missing"), 0);
        assert_eq!(number_at(&v, "bad"), 0);
    }

    #[test]
    fn escape_label_escapes_specials() {
        assert_eq!(escape_label("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        assert_eq!(escape_label("openai"), "openai");
    }

    #[test]
    fn metric_lines_have_help_type_and_sample() {
        let mut out = String::new();
        metric_gauge(&mut out, "aiapi_up", "up?", 1);
        assert!(out.contains("# HELP aiapi_up"));
        assert!(out.contains("# TYPE aiapi_up gauge"));
        assert!(out.contains("aiapi_up 1\n"));
    }
}
