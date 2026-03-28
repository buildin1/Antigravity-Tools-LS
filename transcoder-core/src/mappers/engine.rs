use anyhow::Result;
use tracing::{error, info, warn};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::common::LsConnectionInfo;
use crate::constants::{GRPC_MAX_RETRIES, GRPC_RETRY_DELAY_MS};
use crate::proto::exa::codeium_common_pb::Metadata;
use crate::mappers::{ProtocolMapper, MapperChunk, StreamMetadata};

pub async fn handle_generic_stream<M: ProtocolMapper>(
    req: M::Request,
    conn: LsConnectionInfo,
    stats_mgr: Option<std::sync::Arc<crate::stats::StatsManager>>,
    meta_tx: Option<tokio::sync::oneshot::Sender<StreamMetadata>>,
) -> Result<mpsc::Receiver<MapperChunk>> {
    let (tx, rx) = mpsc::channel(128);

    let config = crate::common::get_runtime_config();
    let metadata = Metadata {
        ide_name: config.ide_name,
        ide_version: config.version.clone(),
        extension_name: config.extension_name,
        extension_version: config.version,
        ..Default::default()
    };

    let model_name = M::get_model(&req).to_string();
    let parsed = M::build_prompt(&req)?;
    let mut prompt = parsed.text;
    let images = parsed.images;
    let media = parsed.media;

    // 🚀 思考语言增强与 VCoT 注入
    let is_lite = model_name.to_lowercase().contains("lite");
    if is_lite {
        // 对于 Lite 模型，通过 Prompt 注入强制开启虚拟思考链 (VCoT)
        prompt = format!("[System Instruction]\nYou must first perform a step-by-step reasoning process within <thought>...</thought> tags before providing your final answer. Please reason in Chinese. (请务必先并在 <thought> 标签内使用中文进行过程推理，然后再给出最终答案)\n\n{}", prompt);
    } else {
        // 对于原生支持 Thinking 的模型，仅注入语言引导
        prompt = format!("[System Instruction]\nPlease reason in Chinese. (请务必使用中文进行思考)\n\n{}", prompt);
    }

    // 🚀 工作区路径解析
    let resolved_workspace = conn.workspace_dir.clone().or_else(|| M::extract_workspace(&req));

    // 尝试建立连接
    let mut client = crate::cascade::CascadeClient::new(
        conn.grpc_addr.clone(),
        metadata.clone(),
        conn.csrf_token.clone().unwrap_or_default(),
        conn.tls_cert.clone(),
        resolved_workspace,
    ).await?;

    // 发起流式请求
    let mut rx_cascade = client.chat_stream(prompt.clone(), conn.resolved_model_id, images, media).await?;

    // 🚀 关键改进：尝试读取第一帧。如果 LS 直接关闭流且 stderr 有 403，则截获
    let first_res = rx_cascade.recv().await;
    match first_res {
        Some(Err(status)) if status.code() == tonic::Code::PermissionDenied => {
             return Err(anyhow::anyhow!("{}", status.message()));
        }
        _ => {}
    }

    // 握手成功或已有数据，启动后台循环处理剩余工作
    tokio::spawn(async move {
        if let Err(e) = run_transcode_loop_with_client::<M>(tx.clone(), conn, stats_mgr, meta_tx, client, rx_cascade, model_name, is_lite, prompt, first_res).await {
            error!("转码引擎内部异常: {:?}", e);
        }
    });

    Ok(rx)
}

/// VCoT 解析器状态
enum VCoTState {
    Outside,
    Inside,
}

async fn run_transcode_loop_with_client<M: ProtocolMapper>(
    tx: mpsc::Sender<MapperChunk>,
    conn: LsConnectionInfo,
    stats_mgr: Option<std::sync::Arc<crate::stats::StatsManager>>,
    mut meta_tx: Option<tokio::sync::oneshot::Sender<StreamMetadata>>,
    mut _client: crate::cascade::CascadeClient,
    mut rx_cascade: mpsc::Receiver<Result<crate::mappers::CascadeDelta, tonic::Status>>,
    model_name: String,
    is_lite: bool,
    prompt: String,
    first_res: Option<Result<crate::mappers::CascadeDelta, tonic::Status>>,
) -> Result<()> {
    let mut tool_call_buffer = String::new();
    let mut in_tool_call = false;
    let mut tool_call_index = 0u32;
    let mut vcot_state = VCoTState::Outside;

    for chunk in M::initial_chunks() {
        let _ = tx.send(chunk).await;
    }

    let mut full_response_content = String::new();

    if let Some(res) = first_res {
        process_one_delta::<M>(&tx, &model_name, is_lite, res, &mut vcot_state, &mut full_response_content, &mut tool_call_buffer, &mut in_tool_call, &mut tool_call_index).await?;
    }

    while let Some(res) = rx_cascade.recv().await {
        process_one_delta::<M>(&tx, &model_name, is_lite, res, &mut vcot_state, &mut full_response_content, &mut tool_call_buffer, &mut in_tool_call, &mut tool_call_index).await?;
    }

    // 🚀 核心兜底逻辑：如果流执行完毕但没有任何内容产出，通常意味着 LS 进程发生了静默报错
    if full_response_content.is_empty() {
         let mut err_msg = "内核返回内容为空，可能触发了 403 权限或网络异常。".to_string();
         if let Some(ref fetcher) = conn.error_fetcher {
             if let Some(raw_err) = fetcher.get_last_error() { err_msg = raw_err; }
         }
         let _ = send_error_to_user::<M>(&tx, &model_name, &err_msg, &mut tool_call_buffer, &mut in_tool_call, &mut tool_call_index).await;
    }

    // 发送结束帧
    let final_chunks = M::map_delta(&model_name, crate::mappers::CascadeDelta::Text(String::new()), true, &mut tool_call_buffer, &mut in_tool_call, &mut tool_call_index).await?;
    for chunk in final_chunks { let _ = tx.send(chunk).await; }

    // 统计量
    let input_tokens = (prompt.len() / 4).max(1) as u32;
    let output_tokens = (full_response_content.len() / 4).max(1) as u32;
    if let Some(mgr) = stats_mgr {
        let account = conn.account_email.clone().unwrap_or_else(|| "anonymous".to_string());
        let _ = mgr.record_usage(&account, &model_name, input_tokens, output_tokens);
    }

    if let Some(meta_tx_inner) = meta_tx.take() {
        let _ = meta_tx_inner.send(crate::mappers::StreamMetadata { input_tokens, output_tokens, error: None });
    }

    Ok(())
}

async fn process_one_delta<M: ProtocolMapper>(
    tx: &mpsc::Sender<MapperChunk>,
    model_name: &str,
    is_lite: bool,
    res: Result<crate::mappers::CascadeDelta, tonic::Status>,
    state: &mut VCoTState,
    full_content: &mut String,
    tc_buf: &mut String,
    in_tc: &mut bool,
    tc_idx: &mut u32,
) -> Result<()> {
    match res {
        Ok(delta) => {
            match delta {
                crate::mappers::CascadeDelta::Thinking(t) => {
                    let chunks = M::map_delta(model_name, crate::mappers::CascadeDelta::Thinking(t), false, tc_buf, in_tc, tc_idx).await?;
                    for chunk in chunks { let _ = tx.send(chunk).await; }
                }
                crate::mappers::CascadeDelta::Text(t) => {
                    if !is_lite {
                        full_content.push_str(&t);
                        let chunks = M::map_delta(model_name, crate::mappers::CascadeDelta::Text(t), false, tc_buf, in_tc, tc_idx).await?;
                        for chunk in chunks { let _ = tx.send(chunk).await; }
                    } else {
                        // 🚀 核心 VCoT 剥离逻辑：拦截 <thought> 标签
                        let mut current_text = t;
                        while !current_text.is_empty() {
                            match state {
                                VCoTState::Outside => {
                                    if let Some(start_pos) = current_text.find("<thought>") {
                                        let prefix = &current_text[..start_pos];
                                        if !prefix.is_empty() {
                                            full_content.push_str(prefix);
                                            let chunks = M::map_delta(model_name, crate::mappers::CascadeDelta::Text(prefix.to_string()), false, tc_buf, in_tc, tc_idx).await?;
                                            for chunk in chunks { let _ = tx.send(chunk).await; }
                                        }
                                        *state = VCoTState::Inside;
                                        current_text = current_text[start_pos + "<thought>".len()..].to_string();
                                    } else {
                                        full_content.push_str(&current_text);
                                        let chunks = M::map_delta(model_name, crate::mappers::CascadeDelta::Text(current_text), false, tc_buf, in_tc, tc_idx).await?;
                                        for chunk in chunks { let _ = tx.send(chunk).await; }
                                        current_text = String::new();
                                    }
                                }
                                VCoTState::Inside => {
                                    if let Some(end_pos) = current_text.find("</thought>") {
                                        let thought_part = &current_text[..end_pos];
                                        if !thought_part.is_empty() {
                                            let chunks = M::map_delta(model_name, crate::mappers::CascadeDelta::Thinking(thought_part.to_string()), false, tc_buf, in_tc, tc_idx).await?;
                                            for chunk in chunks { let _ = tx.send(chunk).await; }
                                        }
                                        *state = VCoTState::Outside;
                                        current_text = current_text[end_pos + "</thought>".len()..].to_string();
                                    } else {
                                        let chunks = M::map_delta(model_name, crate::mappers::CascadeDelta::Thinking(current_text), false, tc_buf, in_tc, tc_idx).await?;
                                        for chunk in chunks { let _ = tx.send(chunk).await; }
                                        current_text = String::new();
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Err(status) => {
            let err_msg = format!("gRPC 错误 [{}]: {}", status.code(), status.message());
            let _ = send_error_to_user::<M>(tx, model_name, &err_msg, tc_buf, in_tc, tc_idx).await;
            Err(anyhow::anyhow!(err_msg))
        }
    }
}

async fn send_error_to_user<M: ProtocolMapper>(
    tx: &mpsc::Sender<MapperChunk>,
    model_name: &str,
    err_msg: &str,
    tool_call_buffer: &mut String,
    in_tool_call: &mut bool,
    tool_call_index: &mut u32,
) -> Result<()> {
    if let Ok(chunks) = M::map_delta(
        model_name,
        crate::mappers::CascadeDelta::Text(err_msg.to_string()),
        false,
        tool_call_buffer,
        in_tool_call,
        tool_call_index,
    ).await {
        for chunk in chunks { let _ = tx.send(chunk).await; }
    }
    Ok(())
}
