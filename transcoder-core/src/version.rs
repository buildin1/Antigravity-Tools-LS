use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use crate::constants::LS_METADATA_IDE_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityVersionInfo {
    /// Transcoder 模拟的目标版本 (如 1.20.5)
    pub simulated_version: String,
    /// 本地机器安装的 App 版本
    pub local_app_version: Option<String>,
    /// 官方最新的最新版本
    pub remote_latest_version: Option<String>,
}

pub struct VersionManager;

impl VersionManager {
    /// 获取全量版本信息
    pub async fn get_all_version_info(custom_path: Option<String>) -> AntigravityVersionInfo {
        let simulated_version = crate::common::get_runtime_version();
        let local_app_version = Self::detect_local_version(custom_path);
        let remote_latest_version = Self::fetch_remote_version().await;

        AntigravityVersionInfo {
            simulated_version,
            local_app_version,
            remote_latest_version,
        }
    }

    /// 探测本地安装的 Antigravity 版本
    fn detect_local_version(custom_path: Option<String>) -> Option<String> {
        // 1. 优先级：外部传入路径 -> 配置文件已保存路径
        let target_path = custom_path
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| crate::common::get_saved_antigravity_path());

        if let Some(path) = target_path {
            info!("🔍 正在从路径探测版本信息: {:?}", path);
            if path.exists() {
                #[cfg(target_os = "macos")]
                {
                    // 如果指向 .app，自动进入 Contents/Info.plist
                    let plist_path = if path.extension().and_then(|s| s.to_str()) == Some("app") {
                        path.join("Contents/Info.plist")
                    } else if path.ends_with("Contents/MacOS/Antigravity") {
                        path.parent().and_then(|p| p.parent()).map(|p| p.join("Info.plist")).unwrap_or(path)
                    } else {
                        path
                    };

                    if let Ok(content) = std::fs::read(&plist_path) {
                        if let Ok(plist) = plist::Value::from_reader(std::io::Cursor::new(content)) {
                            if let Some(dict) = plist.as_dictionary() {
                                if let Some(v) = dict.get("CFBundleShortVersionString").and_then(|v| v.as_string()) {
                                    return Some(v.to_string());
                                }
                            }
                        }
                    }
                }

                #[cfg(not(target_os = "macos"))]
                {
                    // Windows/Linux: 尝试查找同级或 resources/app 下的 package.json
                    let possible_json = if path.is_file() {
                        path.parent().map(|p| p.join("resources/app/package.json")).unwrap_or(path.clone())
                    } else {
                        path.join("resources/app/package.json")
                    };

                    if let Ok(content) = std::fs::read_to_string(&possible_json) {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                            if let Some(v) = json.get("version").and_then(|v| v.as_str()) {
                                return Some(v.to_string());
                            }
                        }
                    }
                }
            }
        }

        // 2. 兜底默认路径探测
        #[cfg(target_os = "macos")]
        {
            let paths = [
                "/Applications/Antigravity.app/Contents/Info.plist",
                "/Applications/Cursor.app/Contents/Info.plist",
            ];
            for path in &paths {
                if let Ok(content) = std::fs::read(path) {
                    if let Ok(plist) = plist::Value::from_reader(std::io::Cursor::new(content)) {
                        if let Some(dict) = plist.as_dictionary() {
                            if let Some(v) = dict.get("CFBundleShortVersionString").and_then(|v| v.as_string()) {
                                return Some(v.to_string());
                            }
                        }
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(user_profile) = std::env::var("USERPROFILE") {
                let path = std::path::Path::new(&user_profile)
                    .join("AppData\\Local\\Programs\\Antigravity\\resources\\app\\package.json");
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(v) = json.get("version").and_then(|v| v.as_str()) {
                            return Some(v.to_string());
                        }
                    }
                }
            }
        }

        None
    }

    async fn fetch_remote_version() -> Option<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36")
            .build()
            .ok()?;

        // 核心修正：使用官方自动更新 JSON 接口 (fetch_official_assets.sh 同款)
        // 网页版 releases 只是个 SPA 壳子，不包含版本号文本。
        let primary_url = "https://antigravity-auto-updater-974169037036.us-central1.run.app/releases";
        match client.get(primary_url)
            .header("Accept", "application/json")
            .header("Accept-Encoding", "identity")
            .send().await {
            Ok(resp) => {
                if let Ok(text) = resp.text().await {
                    if let Some(ver) = Self::extract_version(&text) {
                        info!("🌐 通过官方 Releases API 获取到远程版本: {}", ver);
                        return Some(ver);
                    }
                    warn!("⚠️ Releases API 已响应，但未能提取版本号 (内容前100字符: {:.100})", text);
                }
            }
            Err(e) => {
                warn!("⚠️ Releases API 访问失败: {:?}", e);
            }
        }

        None
    }

    /// 提取语义化版本号
    /// 策略: 宽泛匹配任意位置的版本号
    fn extract_version(text: &str) -> Option<String> {
        let re_loose = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
        re_loose.captures(text).map(|caps| caps[1].to_string())
    }
}
