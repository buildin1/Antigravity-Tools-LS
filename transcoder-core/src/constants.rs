pub const LS_METADATA_IDE_NAME: &str = "antigravity";
pub const LS_METADATA_IDE_VERSION: &str = "1.20.6";
pub const LS_METADATA_EXTENSION_NAME: &str = "antigravity";
pub const LS_METADATA_EXTENSION_VERSION: &str = "1.20.6";
pub const LS_METADATA_EXTENSION_PATH: &str = "";
pub const LS_METADATA_LOCALE: &str = "en_US";

// --- Google OAuth2 全局凭据 (统一管理) ---

/// Client 1: antigravity VSCode 扩展专用
pub const GOOGLE_CLIENT_ID_1: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const GOOGLE_CLIENT_SECRET_1: &str = "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";

/// Client 2: 桌面版/通用版
pub const GOOGLE_CLIENT_ID_2: &str = "884354919052-36trc1jjb3tguiac32ov6cod268c5blh.apps.googleusercontent.com";
pub const GOOGLE_CLIENT_SECRET_2: &str = "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";

/// Client 3: Legacy/External 兼容版
pub const GOOGLE_CLIENT_ID_3: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const GOOGLE_CLIENT_SECRET_3: &str = "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";

pub const GOOGLE_OAUTH_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
pub const OAUTH_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";


/// gRPC 重试配置
pub const GRPC_MAX_RETRIES: u32 = 2;
pub const GRPC_RETRY_DELAY_MS: u64 = 1000;
