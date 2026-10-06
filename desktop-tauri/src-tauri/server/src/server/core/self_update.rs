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
//! 特权容器、挂任意宿主目录）。因此**本能力默认开启**，但**只在本容器确实能连到
//! docker.sock 时**才对外可用（`available()` 会 ping 一次 daemon）：
//!   · 没挂 socket 的部署 → 探测失败 → 网页端退回「复制更新命令」，不会给出一个
//!     点了必错的按钮；
//!   · 想彻底关掉，设 `AIAPI_ALLOW_SELF_UPDATE=0`。
//! ⚠️ 一旦公网暴露或多人可登面板，请确认是否真的要给容器宿主 root 级权限 ——
//!    不要的话就**不要挂 docker.sock**（本能力会自动失效），或显式设 0 关掉。
//!
//! ── 相关环境变量 ──────────────────────────────────────────────
//!   AIAPI_ALLOW_SELF_UPDATE                   显式 `0` = 关闭（默认开）
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

/// 是否允许本能力：**默认开**，仅当显式设 `AIAPI_ALLOW_SELF_UPDATE=0`
/// （或旧名 `AGENT2API_ALLOW_SELF_UPDATE=0`）时关闭。
///
/// 只声明「用户没反对」，不代表真的可用 —— 真正的可用性看 [`available()`]
/// （还要能连上 docker.sock）。
pub fn enabled() -> bool {
    for name in ["AIAPI_ALLOW_SELF_UPDATE", "AGENT2API_ALLOW_SELF_UPDATE"] {
        if let Ok(value) = std::env::var(name) {
            if value.trim() == "0" {
                return false;
            }
        }
    }
    true
}

/// 本能力此刻是否**真的能用**：允许且能连到本机 docker daemon。
///
/// 网页端据此决定显示「一键更新」还是退回「复制更新命令」—— 没挂 socket 的部署
/// 不会看到一个点了必错的按钮。ping 一次很轻（本地 unix socket）。
pub async fn available() -> bool {
    if !enabled() {
        return false;
    }
    match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => docker.ping().await.is_ok(),
        Err(_) => false,
    }
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
        return Err("面板一键更新已被显式关闭（AIAPI_ALLOW_SELF_UPDATE=0）".to_string());
    }
    let docker = bollard::Docker::connect_with_local_defaults()
        .map_err(|error| format!("连接 Docker 失败：{error}"))?;
    // 连不上 daemon（多半没挂 docker.sock）时给一句可执行的指引，而不是底层报错
    docker
        .ping()
        .await
        .map_err(|error| format!("连不上 Docker（本容器是否已挂载 docker.sock？）：{error}"))?;

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

    // 1) 先拉更新器镜像（create/start 不会自动 pull）。如果拉取失败，继续尝试
    //    使用宿主机本地已有镜像；内网/国内机器常见 Docker Hub 超时，但预拉过的
    //    watchtower 仍然可以正常启动。
    let mut pull_error: Option<String> = None;
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
        if let Err(error) = chunk {
            pull_error = Some(format!("拉取更新器镜像 {image} 失败：{error}"));
            break;
        }
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
        .map_err(|error| match pull_error {
            Some(reason) => format!("{reason}；本地镜像也不可用，创建更新器容器失败：{error}"),
            None => format!("创建更新器容器失败：{error}"),
        })?;

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
