use axum::{
    extract::State,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use crate::state::AppState;
use transcoder_core::transcoder::VersionManager;

#[derive(Serialize, Deserialize)]
pub struct UpdateCheckResponse {
    pub success: bool,
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub release_url: String,
    pub message: String,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
}

/// GET /v1/version
/// 获取三个维度的 Antigravity 版本信息
pub async fn get_version_info_api(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let custom_path = {
        let settings = state.app_settings.read().await;
        settings.antigravity_executable.clone()
    };

    let info = VersionManager::get_all_version_info(custom_path).await;
    
    Json(json!({
        "success": true,
        "data": info
    })).into_response()
}

/// GET /v1/version/check
/// 检查 Dashboard 系统更新 (对接 Antigravity-Tools 仓库)
pub async fn check_dashboard_updates_api() -> impl IntoResponse {
    let current_version = "0.0.1"; // 当前 Dashboard 体验版基准
    let repo_url = "https://api.github.com/repos/lbjlaq/Antigravity-Tools-LS/releases/latest";
    
    let client = reqwest::Client::builder()
        .user_agent("Antigravity-Dashboard-Updater")
        .timeout(std::time::Duration::from_secs(5))
        .build();

    if let Ok(c) = client {
        match c.get(repo_url).send().await {
            Ok(resp) => {
                if resp.status() == 404 {
                    return Json(UpdateCheckResponse {
                        success: true,
                        current_version: current_version.to_string(),
                        latest_version: "0.0.0".to_string(),
                        has_update: false,
                        release_url: "".to_string(),
                        message: "GitHub 仓库尚未发布任何 Release".to_string(),
                    });
                }

                if let Ok(release) = resp.json::<GitHubRelease>().await {
                    let latest_version = release.tag_name.replace('v', "");
                    let has_update = latest_version != current_version;
                    
                    return Json(UpdateCheckResponse {
                        success: true,
                        current_version: current_version.to_string(),
                        latest_version,
                        has_update,
                        release_url: release.html_url,
                        message: if has_update { "发现新版本".to_string() } else { "已是最新版本".to_string() },
                    });
                }
            }
            Err(e) => {
                tracing::warn!("⚠️ GitHub 更新检查失败: {}", e);
            }
        }
    }

    Json(UpdateCheckResponse {
        success: false,
        current_version: current_version.to_string(),
        latest_version: "UNKNOWN".to_string(),
        has_update: false,
        release_url: "".to_string(),
        message: "更新检查超时或网络异常".to_string(),
    })
}
