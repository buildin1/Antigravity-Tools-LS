use tonic::{transport::{Channel, Endpoint, ClientTlsConfig, Certificate}, Request};
use crate::proto::exa::language_server_pb::{
    language_server_service_client::LanguageServerServiceClient,
    StartCascadeRequest, SendUserCascadeMessageRequest, GetCascadeTrajectoryRequest,
};
use crate::proto::exa::cortex_pb::{CortexTrajectoryType, CascadeRunStatus, CortexStepType, CortexStepStatus};
use crate::proto::exa::codeium_common_pb::{Metadata, TextOrScopeItem};
use crate::proto::exa::reactive_component_pb::{StreamReactiveUpdatesRequest, MessageDiff};
use crate::mappers::CascadeDelta; // 👈 引入新的增量类型
use tokio::time::sleep;
use std::time::Duration;
use tokio_stream::StreamExt;

pub struct CascadeClient {
// ... (omitting new() for brevity, it's already there)
// Wait, I should include the whole impl block or use replace_file_content carefully.

// Let's refine the replacement to the chat_stream method.
    client: LanguageServerServiceClient<Channel>,
    metadata: Metadata,
    auth_token: String,
}

fn save_image_to_disk(image_data: &[u8], mime_type: &str) -> String {
    let mime = if mime_type.is_empty() { "image/png" } else { mime_type };
    let file_id = uuid::Uuid::new_v4();
    let ext = if mime.contains("jpeg") || mime.contains("jpg") { "jpg" } else { "png" };
    let ws_dir = crate::common::get_app_data_dir().join("images");
    let _ = std::fs::create_dir_all(&ws_dir);
    let disk_img_path = ws_dir.join(format!("img_{}.{}", file_id, ext));
    let disk_b64_path = ws_dir.join(format!("img_{}.b64.txt", file_id));
    
    let _ = std::fs::write(&disk_img_path, image_data);
    
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(image_data);
    let data_url = format!("data:{};base64,{}", mime, b64);
    let _ = std::fs::write(&disk_b64_path, &data_url);

    format!(
        "\n\n![Generated Image]({})\n\n*(图片亦保存至本地: `{}`，Base64格式备份于: `{}`)*\n\n",
        disk_img_path.display(), disk_img_path.display(), disk_b64_path.display()
    )
}

/// 从 Reactive Diff 中递归提取文本增量 (针对 PlannerResponse.response)
/// Trajectory (1: steps) -> Step (20: planner_response) -> PlannerResponse (1: response)
fn extract_text_from_diff(diff: &MessageDiff, path: &[u32]) -> Option<String> {
    if path.is_empty() { return None; }
    let target_field = path[0];
    
    for fd in &diff.field_diffs {
        if fd.field_number == target_field {
            use crate::proto::exa::reactive_component_pb::field_diff::Diff;
            match &fd.diff {
                Some(Diff::UpdateSingular(sv)) => {
                    use crate::proto::exa::reactive_component_pb::singular_value::Value;
                    match &sv.value {
                        Some(Value::StringValue(s)) if path.len() == 1 => return Some(s.clone()),
                        Some(Value::MessageValue(inner_diff)) if path.len() > 1 => {
                            return extract_text_from_diff(inner_diff, &path[1..]);
                        }
                        _ => {}
                    }
                }
                Some(Diff::UpdateRepeated(rd)) => {
                    // 对于 steps (field 1)，通常在最后追加
                    if target_field == 1 && path.len() > 1 {
                        for val in rd.update_values.iter().rev() {
                            use crate::proto::exa::reactive_component_pb::singular_value::Value;
                            if let Some(Value::MessageValue(inner_diff)) = &val.value {
                                if let Some(txt) = extract_text_from_diff(inner_diff, &path[1..]) {
                                    return Some(txt);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    None
}

impl CascadeClient {
    pub async fn new(
        grpc_addr: String,
        metadata: Metadata,
        csrf_token: String,
        tls_cert: Vec<u8>,
        workspace_dir: Option<String>,
    ) -> anyhow::Result<Self> {
        let addr = if grpc_addr.starts_with("http") { grpc_addr } else { format!("https://{}", grpc_addr) };
        
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(&tls_cert))
            .domain_name("localhost");

        let channel = Endpoint::from_shared(addr)?
            .tls_config(tls)?
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_while_idle(true)
            .connect_timeout(Duration::from_secs(10))
            .connect()
            .await?;
            
        let mut client = LanguageServerServiceClient::new(channel);

        // 🚀 如果有工作区目录，向 LS 引擎注册工作区
        // 注意两个 gRPC 方法对路径格式的要求不同：
        //   AddTrackedWorkspace 要求裸绝对路径（如 /Users/xxx/cse3）
        //   SetWorkingDirectories 要求 file:// URI 格式（如 file:///Users/xxx/cse3）
        if let Some(ref dir) = workspace_dir {
            if !dir.is_empty() {
                let bare_path = dir.trim_start_matches("file://").to_string();
                let uri = if bare_path.starts_with('/') {
                    format!("file://{}", bare_path)
                } else {
                    format!("file:///{}", bare_path)
                };

                // AddTrackedWorkspace: 裸绝对路径
                let add_ws_req = crate::proto::exa::language_server_pb::AddTrackedWorkspaceRequest {
                    workspace: bare_path.clone(),
                    do_not_watch_files: true,
                    is_passive_workspace: false,
                };
                let mut grpc_req = Request::new(add_ws_req);
                grpc_req.metadata_mut().insert("x-codeium-csrf-token", csrf_token.parse().unwrap());
                if let Err(e) = client.add_tracked_workspace(grpc_req).await {
                    tracing::warn!("⚠️ [Cascade] 注册工作区失败 ({}): {:?}", bare_path, e);
                } else {
                    tracing::info!("✅ [Cascade] 工作区已注册: {}", bare_path);
                }

                // SetWorkingDirectories: file:// URI 格式
                let set_wd_req = crate::proto::exa::language_server_pb::SetWorkingDirectoriesRequest {
                    directory_uris: vec![uri.clone()],
                };
                let mut grpc_req2 = Request::new(set_wd_req);
                grpc_req2.metadata_mut().insert("x-codeium-csrf-token", csrf_token.parse().unwrap());
                if let Err(e) = client.set_working_directories(grpc_req2).await {
                    tracing::warn!("⚠️ [Cascade] 设置工作目录失败 ({}): {:?}", uri, e);
                } else {
                    tracing::info!("✅ [Cascade] 工作目录已设置: {}", uri);
                }
            }
        }

        Ok(Self {
            client,
            metadata,
            auth_token: csrf_token,
        })
    }

    fn auth_request<T>(&self, msg: T) -> Request<T> {
        let mut req = Request::new(msg);
        req.metadata_mut().insert("x-codeium-csrf-token", self.auth_token.parse().unwrap());
        req
    }

    /// 发起一次 Cascade 对话并开启流式增量输出
    pub async fn chat_stream(
        &mut self, 
        user_text: String, 
        model_id: i32, 
        images: Vec<crate::proto::exa::codeium_common_pb::ImageData>,
        media: Vec<crate::proto::exa::codeium_common_pb::Media>,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<CascadeDelta, tonic::Status>>, anyhow::Error> {
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        let model_enum = model_id;
        let mut client_clone = self.client.clone();
        let csrf_token_clone = self.auth_token.clone();

        // 构造统一的 CascadeConfig
        let cascade_config = Some(crate::proto::exa::cortex_pb::CascadeConfig {
            planner_config: Some(crate::proto::exa::cortex_pb::CascadePlannerConfig {
                requested_model: Some(crate::proto::exa::codeium_common_pb::ModelOrAlias {
                    choice: Some(crate::proto::exa::codeium_common_pb::model_or_alias::Choice::Model(model_enum)),
                }),
                planner_type_config: Some(crate::proto::exa::cortex_pb::cascade_planner_config::PlannerTypeConfig::Conversational(
                    crate::proto::exa::cortex_pb::CascadeConversationalPlannerConfig::default()
                )),
                ..Default::default()
            }),
            ..Default::default()
        });

        // 1. StartCascade
        let start_req = StartCascadeRequest {
            metadata: Some(self.metadata.clone()),
            trajectory_type: CortexTrajectoryType::Cascade as i32,
            ..Default::default()
        };
        let start_resp = self.client.start_cascade(self.auth_request(start_req)).await.map_err(|status| {
            tracing::error!("❌ [Cascade] StartCascade failed. Status: {:?}, Metadata: {:?}", status, status.metadata());
            anyhow::anyhow!("StartCascade 错误 [{}]: {}", status.code(), status.message())
        })?.into_inner();
        let cascade_id = start_resp.cascade_id;
        tracing::info!("🚀 [Cascade] 已启动会话 (流流结合): {}", cascade_id);

        if !images.is_empty() {
            tracing::info!("📸 [Cascade] 当前请求包含 {} 张图片(Base64) 和 {} 个媒体对象", images.len(), media.len());
        }

        // 2. SendUserCascadeMessage
        let send_req = SendUserCascadeMessageRequest {
            metadata: Some(self.metadata.clone()),
            cascade_id: cascade_id.clone(),
            items: vec![TextOrScopeItem {
                chunk: Some(crate::proto::exa::codeium_common_pb::text_or_scope_item::Chunk::Text(user_text)),
            }],
            images,
            media,
            cascade_config,
            ..Default::default()
        };
        self.client.send_user_cascade_message(self.auth_request(send_req)).await.map_err(|status| {
            tracing::error!("❌ [Cascade] SendUserCascadeMessage failed. Status: {:?}, Metadata: {:?}", status, status.metadata());
            anyhow::anyhow!("SendUserCascadeMessage 错误 [{}]: {}", status.code(), status.message())
        })?;

        // 3. 轮询 Fallback 与 最终快照确认
        tokio::spawn(async move {
            let mut last_text_sent_len = 0;
            let mut last_thinking_sent_len = 0;
            let mut retry_count = 0;

            loop {
                let mut req = Request::new(GetCascadeTrajectoryRequest {
                    cascade_id: cascade_id.clone(),
                    ..Default::default()
                });
                req.metadata_mut().insert("x-codeium-csrf-token", csrf_token_clone.parse().unwrap());

                match client_clone.get_cascade_trajectory(req).await {
                    Ok(resp) => {
                        let traj_resp = resp.into_inner();
                        if let Some(traj) = &traj_resp.trajectory {
                            // 查找最新的 PlannerResponse
                            let planner_res = traj.steps.iter().rev()
                                .filter(|s| s.r#type == CortexStepType::PlannerResponse as i32)
                                .find_map(|s| {
                                    if let Some(crate::proto::gemini_coder::step::Step::PlannerResponse(pr)) = &s.step {
                                        Some(pr)
                                    } else { None }
                                });

                            if let Some(pr) = planner_res {
                                // 处理正文 Text
                                if pr.response.len() > last_text_sent_len {
                                    let delta = &pr.response[last_text_sent_len..];
                                    if tx.send(Ok(CascadeDelta::Text(delta.to_string()))).await.is_err() { break; }
                                    last_text_sent_len = pr.response.len();
                                }
                                // 处理思考链 Thinking (字段 3)
                                if pr.thinking.len() > last_thinking_sent_len {
                                    let delta = &pr.thinking[last_thinking_sent_len..];
                                    if tx.send(Ok(CascadeDelta::Thinking(delta.to_string()))).await.is_err() { break; }
                                    last_thinking_sent_len = pr.thinking.len();
                                }
                            }

                            // 状态退出逻辑
                            if traj_resp.status == CascadeRunStatus::Idle as i32 && retry_count > 10 {
                                break;
                            }
                        }
                        
                        if retry_count > 120 { // 约 1 分钟超时
                             break;
                        }
                    }
                    Err(s) => {
                        if retry_count < 5 {
                            sleep(Duration::from_secs(1 << retry_count)).await;
                        } else {
                            let _ = tx.send(Err(s)).await;
                            break;
                        }
                    }
                }

                retry_count += 1;
                sleep(Duration::from_millis(500)).await;
            }
        });

        Ok(rx)
    }
}
