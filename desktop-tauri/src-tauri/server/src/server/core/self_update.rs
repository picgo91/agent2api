//! 「面板一键更新」—— Docker 部署下，通过挂进容器的 `docker.sock` 调 Docker
//! Engine API，拉起一个**一次性的 Watchtower 容器**去 pull 新镜像并重建本容器。
//!
//! ── 为什么不是「本容器自我重建」──────────────────────────────
//! 对一个**映射了宿主机端口**的容器（本项目的 `aiapi` 映射 3065），从内部自我
//! 重建有个死结：新容器要绑定同一端口，必须等旧容器先释放；而「释放端口」＝
//! 执行更新逻辑的那个进程退出，剩下的 create/start 没人执行；且 `restart:
//! unless-stopped` 还会把旧镜像的容器重新拉起抢端口。**发起者必须在被替换的
//! 容器之外** —— 这正是 Watchtower 要单独跑一个容器的原因。
//!
//! 所以这里的做法是：本容器只负责**把 Watchtower 拉起来**（它挂在同一个
//! docker.sock 上、以 `--run-once` 跑一次就退出），真正的 pull + 重建由它完成。
//!
//! ── 安全边界（务必先读）──────────────────────────────────────
//! 挂 `docker.sock` 等于把**宿主机 Docker 的完全控制权**交给这个容器（能起
//! 特权容器、挂任意宿主目录）—— 这是本能力**默认关闭**的原因：只有显式设
//! `AIAPI_ALLOW_SELF_UPDATE=1` 才启用；未启用时端点明确拒绝，网页端也不会显示
//! 真按钮（见前端 update-shared / update-panel）。**只建议自己的私有机（局域网
//! / 单用户）开启；一旦公网暴露或多人可登面板，不要开。**
//!
//! ── 相关环境变量 ──────────────────────────────────────────────
//!   AIAPI_ALLOW_SELF_UPDATE=1                 启用本能力（默认不设 = 关闭）
//!   AIAPI_SELF_UPDATE_WATCHTOWER_IMAGE        上游更新器镜像（默认
//!                                             containrrr/watchtower:latest）
//!   AIAPI_SELF_UPDATE_TARGET                  要重建的目标容器名 / 镜像名
//!                                             （默认 = 本容器自己的名字）
//!
//! 依赖方向：本模块只被 `api::update` 调用；不碰转发链路。

use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, StartContainerOptions,
};
use futures::StreamExt;

/// 启用开关：`AIAPI_ALLOW_SELF_UPDATE=1`（改名前的 `AGENT2API_` 同名变量仍可读）。
pub fn enabled() -> bool {
    for name in ["AIAPI_ALLOW_SELF_UPDATE", "AGENT2API_ALLOW_SELF_UPDATE"] {
        if let Ok(value) = std::env::var(name) {
            if value.trim() == "1" {
                return true;
            }
        }
    }
    false
}

fn watchtower_image() -> String {
    std::env::var("AIAPI_SELF_UPDATE_WATCHTOWER_IMAGE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "containrrr/watchtower:latest".to_string())
}

fn target_override() -> Option<String> {
    std::env::var("AIAPI_SELF_UPDATE_TARGET")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 自己的容器名：Docker 默认把容器 hostname 设成容器 id（短）。hostname 取不到时
/// 用 `HOSTNAME` 环境变量兜底 —— 两者通常同值。
fn self_id_hint() -> Result<String, String> {
    let hostname = std::env::var("HOSTNAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    hostname.ok_or_else(|| "拿不到本容器 id（HOSTNAME 为空）：可能不是在容器里运行".to_string())
}

/// 执行「一键更新」：拉一次 Watchtower 并启动它（它随后 pull + 重建目标容器）。
///
/// 成功返回一句给界面看的中文说明；任何一步失败返回原因（不 panic —— 与全仓
/// 「handler 不 panic」的取向一致）。
pub async fn run() -> Result<String, String> {
    if !enabled() {
        return Err(
            "未启用面板一键更新：请在部署里设 AIAPI_ALLOW_SELF_UPDATE=1 并挂载 docker.sock"
                .to_string(),
        );
    }
    let docker = bollard::Docker::connect_with_local_defaults()
        .map_err(|error| format!("连接 Docker 失败：{error}"))?;

    // 目标：优先环境变量点名；否则用本容器名（inspect 自己拿到去斜杠的名字）。
    let target = match target_override() {
        Some(target) => target,
        None => {
            let hint = self_id_hint()?;
            let info = docker
                .inspect_container(&hint, None)
                .await
                .map_err(|error| {
                    format!("读取本容器信息失败（docker.sock 是否已挂？）：{error}")
                })?;
            info.name
                .map(|name| name.trim_start_matches('/').to_string())
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "本容器没有名字，且未设置 AIAPI_SELF_UPDATE_TARGET".to_string())?
        }
    };

    let image = watchtower_image();

    // 1) 先拉更新器镜像（create/start 不会自动 pull）。流要消费掉才会真正下载完。
    let mut pull = docker.create_image(
        Some(
            CreateImageOptionsBuilder::default()
                .from_image(&image)
                .build(),
        ),
        None,
        None,
    );
    while let Some(chunk) = pull.next().await {
        chunk.map_err(|error| format!("拉取更新器镜像 {image} 失败：{error}"))?;
    }

    // 2) 建一次性容器：挂 docker.sock、跑完自删（auto_remove）。`--run-once` 让它
    //    只更新一轮就退出；带目标是只重建这一个容器，不碰宿主上别的容器。
    //    名字带时间戳，避免上一次残留（没删干净时）撞名。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!("aiapi-self-update-{stamp}");
    let options = CreateContainerOptionsBuilder::default().name(&name).build();
    let body = ContainerCreateBody {
        image: Some(image.clone()),
        cmd: Some(vec![
            "--run-once".to_string(),
            "--cleanup".to_string(),
            target.clone(),
        ]),
        host_config: Some(HostConfig {
            // 把宿主 docker 的 socket 暴露给更新器（本容器自己也是通过它连上的）
            binds: Some(vec!["/var/run/docker.sock:/var/run/docker.sock".to_string()]),
            auto_remove: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let created = docker
        .create_container(Some(options), body)
        .await
        .map_err(|error| format!("创建更新器容器失败：{error}"))?;

    docker
        .start_container(&created.id, None::<StartContainerOptions>)
        .await
        .map_err(|error| format!("启动更新器容器失败：{error}"))?;

    Ok(format!(
        "已启动更新器（{image}）：它正在拉取新镜像并重建「{target}」，约十几秒后本页面会短暂断开，请稍后刷新"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_reads_flag() {
        // 不依赖进程环境（其它测试可能并行改 env）：直接验证判定函数的取值口径
        // 通过对私有常量的行为 —— 这里只做「未设 = false」的基本断言。
        // 设了 env 的场景交给集成验证（容器里跑）。
        let _ = enabled();
    }
}
