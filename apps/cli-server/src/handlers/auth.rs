use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect},
    Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info};
use crate::state::AppState;

use transcoder_core::constants::*;

#[derive(Deserialize)]
pub struct AuthQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct RefreshTokenReq {
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct CallbackUrlReq {
    pub url: String,
}

#[derive(Deserialize)]
pub struct ManualImportReq {
    pub refresh_token: String,
    pub email: Option<String>,
}

pub async fn auth_login(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let state_uuid = uuid::Uuid::new_v4().to_string();
    
    // 将状态存入缓存以备回调校验 (一键作废)
    {
        let mut states = state.auth_states.write().await;
        states.insert(state_uuid.clone());
    }

    let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| format!("http://localhost:{}", state.port));
    let redirect_uri = format!("{}/oauth-callback", base_url);
    let params = [
        ("client_id", GOOGLE_CLIENT_ID_2),
        ("redirect_uri", &redirect_uri),
        ("response_type", "code"),
        ("scope", GOOGLE_OAUTH_SCOPES),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("state", &state_uuid),
    ];
    let url = url::Url::parse_with_params(OAUTH_AUTH_URL, &params).unwrap();
    
    info!("🔗 唤起浏览器完成 Google OAuth 授权 [State={}]: {}", state_uuid, url);
    let _ = open::that(url.as_str());
    
    Redirect::to(url.as_str())
}

pub async fn oauth_callback(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AuthQuery>,
) -> impl IntoResponse {
    if let Some(err) = query.error {
        return (StatusCode::BAD_REQUEST, format!("授权失败: {}", err)).into_response();
    }
    
    // 1. 严格校验 State 并立即销毁 (防止链接重用)
    let state_val = match query.state {
        Some(s) => s,
        None => return (StatusCode::FORBIDDEN, "缺少安全校验码 (state)").into_response(),
    };

    {
        let mut states = state.auth_states.write().await;
        if !states.remove(&state_val) {
            error!("🚫 发现非法或已过期的授权回调尝试: {}", state_val);
            return (StatusCode::FORBIDDEN, "授权超时或链接已失效，请重新登录").into_response();
        }
    }

    let code = match query.code {
        Some(c) => c,
        None => return (StatusCode::BAD_REQUEST, "缺少授权码").into_response(),
    };

    let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| format!("http://localhost:{}", state.port));
    let redirect_uri = format!("{}/oauth-callback", base_url);
    let params = [
        ("client_id", GOOGLE_CLIENT_ID_2),
        ("client_secret", GOOGLE_CLIENT_SECRET_2),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("grant_type", "authorization_code"),
    ];

    match state.http_client.post(OAUTH_TOKEN_URL)
        .form(&params)
        .send().await {
        Ok(resp) => {
            let token_data: serde_json::Value = resp.json().await.unwrap_or_default();
            
            // 尝试从 ID Token 中提取邮箱
            let email = if let Some(id_token) = token_data.get("id_token").and_then(|v| v.as_str()) {
                extract_email_from_id_token(id_token).unwrap_or_else(|| "unknown@google.com".into())
            } else {
                "unknown@google.com".into()
            };

            let oauth_token = ls_accounts::OAuthToken {
                access_token: token_data.get("access_token").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                refresh_token: token_data.get("refresh_token").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                expires_in: token_data.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(3600),
                token_type: token_data.get("token_type").and_then(|v| v.as_str()).unwrap_or("Bearer").to_string(),
                updated_at: chrono::Utc::now(),
            };

            let refresh_token_clone = oauth_token.refresh_token.clone();
            
            let account = ls_accounts::Account {
                id: format!("{:x}", md5::compute(email.as_bytes())), 
                email: email.clone(),
                name: None,
                token: oauth_token,
                status: ls_accounts::AccountStatus::Active,
                disabled_reason: None,
                project_id: None,
                label: None,
                is_proxy_disabled: false,
                created_at: chrono::Utc::now().timestamp(),
                last_used: chrono::Utc::now().timestamp(),
                quota: None,
                device_profile: None,
            };

            if let Err(e) = state.account_manager.upsert_account(account).await {
                error!("❌ 保存账号失败: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("保存账号失败: {}", e)).into_response();
            } else {
                info!("✅ 账号 {} 已成功保存 (去重新增覆盖)", email);
                // 🚀 同步触发首次额度同步，确保返回时已有数据
                let _ = crate::handlers::probes::refresh_quota_internal(state.clone(), refresh_token_clone).await;
                // 🚀 发送变更通知
                let _ = state.account_tx.send("imported".to_string());
            }

            // [MODIFIED] 返回 HTML 辅助页面而非直接重定向，以便在公网环境下辅助用户
            axum::response::Html(format!(r#"
                <!DOCTYPE html>
                <html>
                <head>
                    <meta charset="UTF-8">
                    <title>授权成功 - Antigravity</title>
                    <style>
                        body {{ font-family: -apple-system, sans-serif; display: flex; align-items: center; justify-content: center; height: 100vh; margin: 0; background: #f0f4f8; }}
                        .card {{ background: white; padding: 40px; border-radius: 16px; shadow: 0 4px 6px rgba(0,0,0,0.1); text-align: center; max-width: 400px; }}
                        .icon {{ font-size: 48px; color: #4caf50; margin-bottom: 20px; }}
                        h2 {{ margin: 0 0 10px; color: #333; }}
                        p {{ color: #666; font-size: 14px; line-height: 1.5; }}
                        .code-box {{ background: #f5f5f5; padding: 12px; border-radius: 8px; font-family: monospace; font-size: 12px; margin: 20px 0; border: 1px dashed #ccc; word-break: break-all; }}
                        .btn {{ background: #2196f3; color: white; border: none; padding: 10px 20px; border-radius: 6px; cursor: pointer; text-decoration: none; font-size: 14px; }}
                    </style>
                </head>
                <body>
                    <div class="card">
                        <div class="icon">✅</div>
                        <h2>授权成功</h2>
                        <p>账号 {email} 已关联到您的服务实例。正在尝试自动关闭窗口...</p>
                        <div class="code-box" id="code">{code}</div>
                        <button class="btn" onclick="copyCode()">复制 Code 并手动提交</button>
                    </div>
                    <script>
                        // 尝试传送消息给父窗口 (Web Dashboard 模式)
                        if (window.opener) {{
                            window.opener.postMessage({{ type: 'oauth-success' }}, '*');
                            // 延迟关闭
                            setTimeout(() => {{ window.close(); }}, 2000);
                        }} else {{
                            // 如果是独立打开的，尝试重定向回首页
                            setTimeout(() => {{ window.location.href = "/"; }}, 5000);
                        }}

                        function copyCode() {{
                            var text = document.getElementById('code').innerText;
                            navigator.clipboard.writeText(text).then(() => {{
                                alert('Code 已复制！请在管理页面的手动登录框中粘贴提交。');
                            }});
                        }}
                    </script>
                </body>
                </html>
            "#, email = email, code = code)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("换取 Token 失败: {}", e)).into_response(),
    }
}

// 极其简易的 JWT Email 提取逻辑 (仅用于非关键流程)
fn extract_email_from_id_token(id_token: &str) -> Option<String> {
    use base64::Engine;
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() < 2 { return None; }
    let payload_b64 = parts[1];
    let payload_bytes = match base64::prelude::BASE64_URL_SAFE_NO_PAD.decode(payload_b64) {
        Ok(b) => b,
        Err(_) => return None,
    };
    let payload_json = String::from_utf8_lossy(&payload_bytes);
    let v: serde_json::Value = serde_json::from_str(&payload_json).ok()?;
    v.get("email").and_then(|v| v.as_str()).map(|s| s.to_string())
}

pub async fn refresh_token_api(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RefreshTokenReq>,
) -> impl IntoResponse {
    let params = [
        ("client_id", GOOGLE_CLIENT_ID_2),
        ("client_secret", GOOGLE_CLIENT_SECRET_2),
        ("refresh_token", &payload.refresh_token),
        ("grant_type", "refresh_token"),
    ];

    match state.http_client.post(OAUTH_TOKEN_URL)
        .form(&params)
        .send().await {
        Ok(resp) => {
            let status = resp.status();
            let data: serde_json::Value = resp.json().await.unwrap_or_default();
            (status, Json(data)).into_response()
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// 帮助函数: 使用 Refresh Token 换取 Access Token (智能匹配三 Client)
#[allow(dead_code)]
pub async fn get_access_token(state: &Arc<AppState>, refresh_token: &str) -> Option<String> {
    // 智能排序：根据 Token 前缀识别来源，减少无效尝试
    let combinations = if refresh_token.starts_with("1//0e") {
        // 1//0e 开头，优先尝试 Client 2 桌面版
        vec![
            (GOOGLE_CLIENT_ID_2, GOOGLE_CLIENT_SECRET_2),
            (GOOGLE_CLIENT_ID_1, GOOGLE_CLIENT_SECRET_1),
            (GOOGLE_CLIENT_ID_3, GOOGLE_CLIENT_SECRET_3),
        ]
    } else if refresh_token.starts_with("1//09") {
        // 1//09 开头，优先尝试 Client 1 插件版
        vec![
            (GOOGLE_CLIENT_ID_1, GOOGLE_CLIENT_SECRET_1),
            (GOOGLE_CLIENT_ID_3, GOOGLE_CLIENT_SECRET_3),
            (GOOGLE_CLIENT_ID_2, GOOGLE_CLIENT_SECRET_2),
        ]
    } else {
        // 其它
        vec![
            (GOOGLE_CLIENT_ID_1, GOOGLE_CLIENT_SECRET_1),
            (GOOGLE_CLIENT_ID_2, GOOGLE_CLIENT_SECRET_2),
            (GOOGLE_CLIENT_ID_3, GOOGLE_CLIENT_SECRET_3),
        ]
    };

    for (cid, csec) in combinations {
        let params = [
            ("client_id", cid),
            ("client_secret", csec),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ];

        match state.http_client.post(OAUTH_TOKEN_URL)
            .form(&params)
            .send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(at) = data.get("access_token").and_then(|v| v.as_str()) {
                            return Some(at.to_string());
                        }
                    }
                } else if status == StatusCode::UNAUTHORIZED {
                    // 该 Client 不匹配，继续尝试下一个
                } else {
                    let err_text = resp.text().await.unwrap_or_default();
                    tracing::error!("OAuth HTTP 错误 {}: {}", status, err_text);
                }
            }
            Err(e) => {
                tracing::error!("OAuth 网络请求完全失败: {}", e);
            }
        }
    }
    None
}

// [NEW] 增强版：获取完整的 OAuthToken (包含 expires_in, updated_at 等)
pub async fn get_access_token_full(state: &Arc<AppState>, refresh_token: &str) -> Option<ls_accounts::OAuthToken> {
    let combinations = if refresh_token.starts_with("1//0e") {
        vec![
            (GOOGLE_CLIENT_ID_2, GOOGLE_CLIENT_SECRET_2),
            (GOOGLE_CLIENT_ID_1, GOOGLE_CLIENT_SECRET_1),
            (GOOGLE_CLIENT_ID_3, GOOGLE_CLIENT_SECRET_3),
        ]
    } else if refresh_token.starts_with("1//09") {
        vec![
            (GOOGLE_CLIENT_ID_1, GOOGLE_CLIENT_SECRET_1),
            (GOOGLE_CLIENT_ID_3, GOOGLE_CLIENT_SECRET_3),
            (GOOGLE_CLIENT_ID_2, GOOGLE_CLIENT_SECRET_2),
        ]
    } else {
        vec![
            (GOOGLE_CLIENT_ID_1, GOOGLE_CLIENT_SECRET_1),
            (GOOGLE_CLIENT_ID_2, GOOGLE_CLIENT_SECRET_2),
            (GOOGLE_CLIENT_ID_3, GOOGLE_CLIENT_SECRET_3),
        ]
    };

    for (cid, csec) in combinations {
        let params = [
            ("client_id", cid),
            ("client_secret", csec),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ];

        match state.http_client.post(OAUTH_TOKEN_URL)
            .header("User-Agent", crate::constants::DEFAULT_USER_AGENT)
            .form(&params)
            .send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                        return Some(ls_accounts::OAuthToken {
                            access_token: data.get("access_token").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                            refresh_token: refresh_token.to_string(),
                            expires_in: data.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(3600),
                            token_type: data.get("token_type").and_then(|v| v.as_str()).unwrap_or("Bearer").to_string(),
                            updated_at: chrono::Utc::now(),
                        });
                    }
                } else if status == StatusCode::UNAUTHORIZED {
                    tracing::warn!("⚠️ Client ID 不匹配或未授权 [CID={}]，切换下一组...", cid);
                    continue;
                } else if status == StatusCode::BAD_REQUEST {
                    let err_text = resp.text().await.unwrap_or_default();
                    tracing::error!("🛑 Google 拒绝刷新请求 (invalid_grant?) [CID={}]: {}", cid, err_text);
                    // 如果是 invalid_grant，通常意味着该 RT 彻底失效，继续尝试其它 Client 已无意义，但为了稳健我们仍完成循环
                } else {
                    let err_text = resp.text().await.unwrap_or_default();
                    tracing::error!("OAuth HTTP 错误 {}: {}", status, err_text);
                }
            }
            Err(e) => {
                tracing::error!("OAuth 网络请求完全失败: {}", e);
            }
        }
    }
    None
}

// [NEW] 手动提交回调 URL 接口 (支持 POST JSON)
pub async fn add_account_by_callback_url_api(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CallbackUrlReq>,
) -> impl IntoResponse {
    add_account_by_callback_url_core(state, payload).await
}

// [NEW] 手动提交回调 URL 接口 (支持 GET Query，方便浏览器直接输出)
pub async fn add_account_by_callback_url_query_api(
    State(state): State<Arc<AppState>>,
    Query(payload): Query<CallbackUrlReq>,
) -> impl IntoResponse {
    add_account_by_callback_url_core(state, payload).await
}

// 核心回调解析逻辑
async fn add_account_by_callback_url_core(
    state: Arc<AppState>,
    payload: CallbackUrlReq,
) -> impl IntoResponse {
    let input_url = payload.url.trim();
    if input_url.is_empty() {
        return (StatusCode::BAD_REQUEST, "URL 不能为空").into_response();
    }

    // 1. 稳健解析 URL。尝试将其解析为完整的 URL。
    // 如果失败（例如用户只粘贴了参数部分 ?code=...），则补全一个 dummy base。
    let parsed_url = if input_url.starts_with("http") {
        match url::Url::parse(input_url) {
            Ok(u) => u,
            Err(e) => return (StatusCode::BAD_REQUEST, format!("非法 URL 格式: {}", e)).into_response(),
        }
    } else {
        let base_prep = if input_url.starts_with('?') || input_url.starts_with('#') {
            format!("http://localhost{}", input_url)
        } else {
            format!("http://localhost?{}", input_url)
        };
        match url::Url::parse(&base_prep) {
            Ok(u) => u,
            Err(_) => return (StatusCode::BAD_REQUEST, "无法将输入识别为有效的 URL 或参数串").into_response(),
        }
    };

    // 2. 从 Query 参数和 Fragment 参数中同时查找 code 和 state
    let mut code = None;
    
    // [NEW] 方案 2.1: 增强解析，如果输入本身就是 bare code (Google 的 code 通常以 4/ 开头)
    if input_url.starts_with("4/") && !input_url.contains('=') && input_url.len() > 30 {
        code = Some(input_url.to_string());
    }

    // 优先从 Query 中找
    if code.is_none() {
        for (k, v) in parsed_url.query_pairs() {
            if k == "code" { code = Some(v.to_string()); break; }
        }
    }
    
    // 如果 Query 中没有，尝试从 Fragment (针对一些 OAuth2 隐式流程或前端跳转的情况) 中找
    if code.is_none() {
        if let Some(fragment) = parsed_url.fragment() {
            for pair in fragment.split('&') {
                let mut parts = pair.splitn(2, '=');
                if let (Some("code"), Some(v)) = (parts.next(), parts.next()) {
                    code = Some(v.to_string());
                    break;
                }
            }
        }
    }

    let code = match code {
        Some(c) => c,
        None => return (StatusCode::BAD_REQUEST, "URL 中未发现授权码 (code)").into_response(),
    };

    // 3. 动态提取 Redirect URI 覆盖
    // 如果用户粘贴的是完整 URL，我们应该提取它的 base 部分作为换码时的凭据。
    // 这是为了解决用户在本地 localhost 登录，但在云端服务器提交导致的 redirect_uri 不匹配报错。
    let redirect_uri_override = if input_url.starts_with("http") {
        let mut base = parsed_url.clone();
        base.set_query(None);
        base.set_fragment(None);
        Some(base.to_string())
    } else {
        None
    };

    match exchange_code_for_account(&state, &code, redirect_uri_override).await {
        Ok(email) => (StatusCode::OK, format!("账号 {} 已通过手动链接导入成功！您可以关闭此窗口。", email)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// [NEW] 直接导入 Refresh Token 接口 (支持 POST JSON)
pub async fn manual_import_account_api(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ManualImportReq>,
) -> impl IntoResponse {
    manual_import_account_core(state, payload).await
}

// [NEW] 直接导入 Refresh Token 接口 (支持 GET Query，方便浏览器直接输出)
pub async fn manual_import_account_query_api(
    State(state): State<Arc<AppState>>,
    Query(payload): Query<ManualImportReq>,
) -> impl IntoResponse {
    manual_import_account_core(state, payload).await
}

// 核心导入逻辑
async fn manual_import_account_core(
    state: Arc<AppState>,
    payload: ManualImportReq,
) -> impl IntoResponse {
    if payload.refresh_token.is_empty() {
        return (StatusCode::BAD_REQUEST, "refresh_token 不能为空").into_response();
    }

    info!("📥 正在尝试手动导入账号 token...");
    
    let token_data = match get_access_token_full(&state, &payload.refresh_token).await {
        Some(t) => t,
        None => return (StatusCode::UNAUTHORIZED, "无效的 Refresh Token 或无法与内置 Client 匹配").into_response(),
    };

    let email = if let Some(e) = payload.email {
        if e.is_empty() { "unknown@google.com".to_string() } else { e }
    } else {
        info!("🔍 正在通过 UserInfo API (v3) 检索账户身份...");
        match state.http_client.get("https://www.googleapis.com/oauth2/v3/userinfo")
            .bearer_auth(&token_data.access_token)
            .send().await {
            Ok(resp) => {
                if let Ok(user_info) = resp.json::<serde_json::Value>().await {
                    user_info.get("email").and_then(|v| v.as_str()).unwrap_or("unknown@google.com").to_string()
                } else {
                    error!("❌ UserInfo 响应解析失败，回退到 unknown");
                    "unknown@google.com".to_string()
                }
            }
            Err(e) => {
                error!("❌ UserInfo API 请求失败: {}，回退到 unknown", e);
                "unknown@google.com".to_string()
            }
        }
    };

    let refresh_token_clone = token_data.refresh_token.clone();
    let account = ls_accounts::Account {
        id: format!("{:x}", md5::compute(email.as_bytes())),
        email: email.clone(),
        name: None,
        token: token_data,
        status: ls_accounts::AccountStatus::Active,
        disabled_reason: None,
        project_id: None,
        label: None,
        is_proxy_disabled: false,
        created_at: chrono::Utc::now().timestamp(),
        last_used: chrono::Utc::now().timestamp(),
        quota: None,
        device_profile: None,
    };

    if let Err(e) = state.account_manager.upsert_account(account).await {
        error!("❌ 手动导入账号失败: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, format!("保存账号失败: {}", e)).into_response()
    } else {
        info!("✅ 账号 {} 已通过 Token 直接导入成功", email);
        // 🚀 同步触发首次额度同步，确保 UI 刷新时数据已就绪
        let _ = crate::handlers::probes::refresh_quota_internal(state.clone(), refresh_token_clone).await;
        // 🚀 发送变更通知
        let _ = state.account_tx.send("imported".to_string());
        (StatusCode::OK, format!("账号 {} 已导入成功！您可以关闭此窗口并返回管理页面。", email)).into_response()
    }
}

async fn exchange_code_for_account(
    state: &Arc<AppState>, 
    code: &str,
    redirect_uri_override: Option<String>
) -> Result<String, String> {
    let redirect_uri = if let Some(uri) = redirect_uri_override {
        uri
    } else {
        let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| format!("http://localhost:{}", state.port));
        format!("{}/oauth-callback", base_url)
    };
    let params = [
        ("client_id", GOOGLE_CLIENT_ID_2),
        ("client_secret", GOOGLE_CLIENT_SECRET_2),
        ("code", code),
        ("redirect_uri", &redirect_uri),
        ("grant_type", "authorization_code"),
    ];

    match state.http_client.post(OAUTH_TOKEN_URL)
        .form(&params)
        .send().await {
        Ok(resp) => {
            let token_data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            let access_token = token_data.get("access_token").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            
            // 尝试 1: 从 ID Token 中高效解析 (JWT 免请求)
            let mut email = if let Some(id_token) = token_data.get("id_token").and_then(|v| v.as_str()) {
                extract_email_from_id_token(id_token)
            } else {
                None
            };

            // 尝试 2: 如果 ID Token 缺失或解析失败，主动调用 UserInfo 接口 (Google 网络请求)
            if email.is_none() || email.as_deref() == Some("unknown@google.com") {
                info!("⚠️ ID Token 缺失或解析失败，尝试通过 UserInfo API 获取身份...");
                match state.http_client.get("https://www.googleapis.com/oauth2/v3/userinfo")
                    .header("Authorization", format!("Bearer {}", access_token))
                    .send().await {
                    Ok(ui_resp) => {
                        if let Ok(ui_data) = ui_resp.json::<serde_json::Value>().await {
                            if let Some(e) = ui_data.get("email").and_then(|v| v.as_str()) {
                                info!("✅ [Fallback] 通过 UserInfo API 成功识别邮箱: {}", e);
                                email = Some(e.to_string());
                            }
                        }
                    }
                    Err(e) => error!("❌ [Fallback] UserInfo API 调用失败: {}", e),
                }
            }

            let email = email.unwrap_or_else(|| "unknown@google.com".to_string());

            let oauth_token = ls_accounts::OAuthToken {
                access_token,
                refresh_token: token_data.get("refresh_token").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                expires_in: token_data.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(3600),
                token_type: token_data.get("token_type").and_then(|v| v.as_str()).unwrap_or("Bearer").to_string(),
                updated_at: chrono::Utc::now(),
            };
            let refresh_token_clone = oauth_token.refresh_token.clone();
            let account = ls_accounts::Account {
                id: format!("{:x}", md5::compute(email.as_bytes())),
                email: email.clone(),
                name: None,
                token: oauth_token,
                status: ls_accounts::AccountStatus::Active,
                disabled_reason: None,
                project_id: None,
                label: None,
                is_proxy_disabled: false,
                created_at: chrono::Utc::now().timestamp(),
                last_used: chrono::Utc::now().timestamp(),
                quota: None,
                device_profile: None,
            };
            state.account_manager.upsert_account(account).await.map_err(|e| e.to_string())?;
            
            // 🚀 同步触发首次额度同步
            let _ = crate::handlers::probes::refresh_quota_internal(state.clone(), refresh_token_clone).await;
            
            // 🚀 发送变更通知
            let _ = state.account_tx.send("imported".to_string());

            Ok(email)

        }
        Err(e) => Err(format!("换取 Token 失败: {}", e)),
    }
}
