use std::path::PathBuf;
use std::fs;
use std::sync::Arc;
use tracing::info;

/// 抽象错误抓取接口，用于从底层实例获取原始 stderr 报错
pub trait ErrorFetcher: Send + Sync {
    fn get_last_error(&self) -> Option<String>;
}

pub struct LsConnectionInfo {
    pub grpc_addr: String,
    pub csrf_token: Option<String>,
    pub access_token: String,
    pub tls_cert: Vec<u8>,
    pub resolved_model_id: i32,
    pub account_email: Option<String>,
    /// 🚀 增强：可选的错误抓取器，用于在流异常结束时回溯原始报错
    pub error_fetcher: Option<Arc<dyn ErrorFetcher>>,
}

pub fn parse_model_enum_string(enum_str: &str) -> i32 {
    if let Some(model_enum) = crate::proto::exa::codeium_common_pb::Model::from_str_name(enum_str) {
        model_enum as i32
    } else {
        if let Ok(id) = enum_str.parse::<i32>() {
            return id;
        }
        0 // MODEL_UNSPECIFIED
    }
}

/// 核心路径探测：获取应用数据的根目录
/// 优先级：1. 环境变量 2. 开发空间自适应 3. ~/.antigravity_tools_ls (默认)
pub fn get_app_data_root() -> PathBuf {
    // 优先级 1: 环境变量显式注入 (用于 Tauri 打包环境)
    if let Ok(env_path) = std::env::var("ANT_TRANSCODER_DATA_DIR") {
        if !env_path.trim().is_empty() {
            let path = PathBuf::from(env_path);
            if !path.exists() { let _ = fs::create_dir_all(&path); }
            return path;
        }
    }

    // 优先级 2: 默认外部路径 ~/.antigravity_tools_ls
    let home = dirs::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let data_root = home.join(".antigravity_tools_ls");
    if !data_root.exists() {
        let _ = fs::create_dir_all(&data_root);
    }
    data_root
}

/// 获取二进制资源目录 (bin)
pub fn get_app_bin_dir() -> PathBuf {
    let bin_dir = get_app_data_root().join("bin");
    if !bin_dir.exists() { let _ = fs::create_dir_all(&bin_dir); }
    bin_dir
}

/// 获取数据持久化目录 (data)
pub fn get_app_data_dir() -> PathBuf {
    let data_dir = get_app_data_root().join("data");
    if !data_dir.exists() { let _ = fs::create_dir_all(&data_dir); }
    data_dir
}

/// 兼容性保留：向上探测项目根目录
pub fn get_project_root() -> PathBuf {
    get_app_data_root()
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsConfig {
    #[serde(default = "default_ls_address")]
    pub ls_address: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_ide_name")]
    pub ide_name: String,
    #[serde(default = "default_extension_name")]
    pub extension_name: String,
    #[serde(default = "default_extension_path")]
    pub extension_path: String,
    #[serde(default = "default_locale")]
    pub locale: String,
}

fn default_ls_address() -> String { "internal-api.antigravity.google:443".to_string() }
fn default_version() -> String { crate::constants::LS_METADATA_IDE_VERSION.to_string() }
fn default_ide_name() -> String { crate::constants::LS_METADATA_IDE_NAME.to_string() }
fn default_extension_name() -> String { crate::constants::LS_METADATA_EXTENSION_NAME.to_string() }
fn default_extension_path() -> String { crate::constants::LS_METADATA_EXTENSION_PATH.to_string() }
fn default_locale() -> String { crate::constants::LS_METADATA_LOCALE.to_string() }

impl Default for LsConfig {
    fn default() -> Self {
        Self {
            ls_address: default_ls_address(),
            version: default_version(),
            ide_name: default_ide_name(),
            extension_name: default_extension_name(),
            extension_path: default_extension_path(),
            locale: default_locale(),
        }
    }
}

/// 🚀 增强：获取当前运行时配置 (优先从外部配置读取)
pub fn get_runtime_config() -> LsConfig {
    let config_path = get_app_data_dir().join("ls_config.json");
    if config_path.exists() {
        if let Ok(content) = fs::read_to_string(config_path) {
            if let Ok(config) = serde_json::from_str::<LsConfig>(&content) {
                return config;
            }
        }
    }
    LsConfig::default()
}

/// 保留兼容性方法，仅返回版本
pub fn get_runtime_version() -> String {
    get_runtime_config().version
}

/// 🚀 增强：从持久化的 app_settings.json 中读取保存的自定义路径
/// 该路径由 cli-server 探测或用户手动选择并保存
pub fn get_saved_antigravity_path() -> Option<PathBuf> {
    let settings_path = get_app_data_dir().join("app_settings.json");
    if settings_path.exists() {
        if let Ok(content) = fs::read_to_string(&settings_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(path_str) = json.get("antigravity_executable").and_then(|v| v.as_str()) {
                    if !path_str.trim().is_empty() {
                        let path = PathBuf::from(path_str);
                        info!("📂 从 app_settings.json 读取到自定义路径: {:?}", path);
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}
