use async_trait::async_trait;
use anyhow::Result;
use serde_json::json;
use crate::mappers::{ProtocolMapper, MapperChunk};
use crate::anthropic::{AnthropicMessageRequest};

pub struct AnthropicMapper;

#[async_trait]
impl ProtocolMapper for AnthropicMapper {
    type Request = AnthropicMessageRequest;

    fn get_protocol() -> String {
        "anthropic".to_string()
    }

    fn get_model(req: &Self::Request) -> &str {
        &req.model
    }

    fn build_prompt(req: &Self::Request) -> Result<crate::mappers::ParsedPrompt> {
        let mut prompt = String::new();
        let mut images = Vec::new();
        let mut media = Vec::new();

        if let Some(tools) = &req.tools {
            let unified_tools = tools.iter().map(|t| crate::tools::UnifiedToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.input_schema.clone().unwrap_or_else(|| json!({})),
            }).collect::<Vec<_>>();
            let tool_prompt = crate::tools::build_tool_system_prompt(&unified_tools);
            if !tool_prompt.is_empty() {
                prompt.push_str(&tool_prompt);
                prompt.push_str("\n\n");
                prompt.push_str("IMPORTANT: If you need to use any of the tools above, you MUST output a <tool_call> XML tag containing the tool name and arguments in JSON format. For example:\n<tool_call>{\"name\": \"tool_name\", \"arguments\": {\"arg1\": \"val1\"}}</tool_call>\nAfter outputting the tag, you should stop generating and wait for the result.\n\n");
            }
        }

        if let Some(sys) = &req.system {
            if let Some(s) = sys.as_str() { prompt.push_str(s); prompt.push_str("\n\n"); }
            else if let Some(arr) = sys.as_array() {
                for block in arr { if let Some(t) = block.get("text").and_then(|v| v.as_str()) { prompt.push_str(t); prompt.push_str("\n"); } }
                prompt.push_str("\n");
            }
        }
        for msg in &req.messages {
            prompt.push_str(&format!("{}: ", msg.role));
            if let Some(s) = msg.content.as_str() { prompt.push_str(s); }
            else if let Some(arr) = msg.content.as_array() {
                for block in arr { 
                    if let Some(type_str) = block.get("type").and_then(|v| v.as_str()) {
                        match type_str {
                            "text" => {
                                if let Some(t) = block.get("text").and_then(|v| v.as_str()) { prompt.push_str(t); }
                            }
                            "image" => {
                                if let Some(source) = block.get("source") {
                                    if source.get("type").and_then(|t| t.as_str()) == Some("base64") {
                                        if let Some(data) = source.get("data").and_then(|d| d.as_str()) {
                                            let mime_type = source.get("media_type").and_then(|m| m.as_str()).unwrap_or("image/jpeg").to_string();
                                            
                                            use base64::Engine as _;
                                            let decoded = base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_default();

                                            images.push(crate::proto::exa::codeium_common_pb::ImageData {
                                                base64_data: data.to_string(),
                                                mime_type: mime_type.clone(),
                                                caption: String::new(),
                                                uri: String::new(),
                                            });
                                            media.push(crate::proto::exa::codeium_common_pb::Media {
                                                mime_type: mime_type.clone(),
                                                description: String::new(),
                                                uri: String::new(),
                                                thumbnail: vec![],
                                                duration_seconds: 0.0,
                                                payload: Some(crate::proto::exa::codeium_common_pb::media::Payload::InlineData(decoded)),
                                            });
                                        }
                                    }
                                }
                            }
                            "tool_use" => {
                                if let Some(n) = block.get("name").and_then(|v| v.as_str()) {
                                    let args = block.get("input").map(|v| v.to_string()).unwrap_or_default();
                                    prompt.push_str(&format!("\n[Assistant Decided to Call Tool '{}' with args '{}']\n", n, args));
                                }
                            }
                            "tool_result" => {
                                prompt.push_str("\n[Tool Execution Result]\n");
                                if let Some(c) = block.get("content") {
                                    if let Some(c_str) = c.as_str() {
                                        prompt.push_str(c_str);
                                    } else if let Some(c_arr) = c.as_array() {
                                        for c_block in c_arr {
                                            if let Some("text") = c_block.get("type").and_then(|v| v.as_str()) {
                                                if let Some(t) = c_block.get("text").and_then(|v| v.as_str()) {
                                                    prompt.push_str(t);
                                                }
                                            }
                                        }
                                    } else {
                                        prompt.push_str(&c.to_string());
                                    }
                                }
                                prompt.push_str("\n");
                            }
                            _ => {}
                        }
                    }
                }
            }
            prompt.push('\n');
        }
        Ok(crate::mappers::ParsedPrompt { text: prompt, images, media })
    }

    fn initial_chunks() -> Vec<MapperChunk> {
        vec![
            MapperChunk {
                event: Some("message_start".into()),
                data: r#"{"type":"message_start","message":{"id":"msg_cascade","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[],"usage":{"input_tokens":0,"output_tokens":0}}}"#.into(),
            },
            MapperChunk {
                event: Some("content_block_start".into()),
                data: r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.into(),
            },
        ]
    }

    async fn map_delta(
        _model: &str,
        delta: crate::mappers::CascadeDelta, // Update to CascadeDelta
        is_final: bool,
        tool_call_buffer: &mut String,
        in_tool_call: &mut bool,
        tool_call_index: &mut u32,
    ) -> Result<Vec<MapperChunk>> {
        let mut results = vec![];

        if is_final {
            results.push(MapperChunk { event: Some("content_block_stop".into()), data: format!(r#"{{"type":"content_block_stop","index":{}}}"#, *tool_call_index * 2) });
            
            let stop_reason = if *tool_call_index > 0 { "tool_use" } else { "end_turn" };
            let message_delta = format!(r#"{{"type":"message_delta","delta":{{"stop_reason":"{}","stop_sequence":null}},"usage":{{"output_tokens":0}}}}"#, stop_reason);
            results.push(MapperChunk { event: Some("message_delta".into()), data: message_delta });
            results.push(MapperChunk { event: Some("message_stop".into()), data: r#"{"type":"message_stop"}"#.into() });
            return Ok(results);
        }

        match delta {
            crate::mappers::CascadeDelta::Thinking(_) => {
                // 🚀 Anthropic 原生协议尚无标准 Reasoning 块，且用户要求不出现在正文。
                // 因此在这里我们跳过 Thinking 块的处理，或者未来可以映射为特定的 content block 类型。
                return Ok(results);
            }
            crate::mappers::CascadeDelta::Text(text) => {
                let mut pending_text = text;
                while !pending_text.is_empty() {
                    if !*in_tool_call {
                        if let Some(start_idx) = pending_text.find("<tool_call>") {
                            let before_text = &pending_text[..start_idx];
                            let current_text_index = *tool_call_index * 2;
                            if !before_text.is_empty() {
                                let delta_json = json!({ "type": "content_block_delta", "index": current_text_index, "delta": { "type": "text_delta", "text": before_text } });
                                results.push(MapperChunk { event: Some("content_block_delta".into()), data: delta_json.to_string() });
                            }
                            *in_tool_call = true;
                            // Stop current text block
                            results.push(MapperChunk { event: Some("content_block_stop".into()), data: format!(r#"{{"type":"content_block_stop","index":{}}}"#, current_text_index) });

                            pending_text = pending_text[start_idx + "<tool_call>".len()..].to_string();
                        } else {
                            let text_index = *tool_call_index * 2;
                            let delta_json = json!({ "type": "content_block_delta", "index": text_index, "delta": { "type": "text_delta", "text": pending_text } });
                            results.push(MapperChunk { event: Some("content_block_delta".into()), data: delta_json.to_string() });
                            pending_text = String::new();
                        }
                    } else {
                        if let Some(end_idx) = pending_text.find("</tool_call>") {
                            let inner_text = &pending_text[..end_idx];
                            tool_call_buffer.push_str(inner_text);
                            
                            let trim_buf = tool_call_buffer.trim();
                            let tool_idx = *tool_call_index * 2 + 1; // Tool block index
                            let next_text_idx = *tool_call_index * 2 + 2; // Next text block index

                            if !trim_buf.is_empty() {
                                if let Ok(json_obj) = serde_json::from_str::<serde_json::Value>(trim_buf) {
                                    let name = json_obj.get("name").and_then(|v| v.as_str()).unwrap_or("unknown_tool").to_string();
                                    let args = json_obj.get("arguments").cloned().unwrap_or_else(|| json!({}));
                                    
                                    results.push(MapperChunk {
                                        event: Some("content_block_start".into()),
                                        data: json!({ "type": "content_block_start", "index": tool_idx, "content_block": { "type": "tool_use", "id": format!("toolu_cascade_{}", *tool_call_index), "name": name, "input": {} } }).to_string(),
                                    });
                                    
                                    let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                                    results.push(MapperChunk {
                                        event: Some("content_block_delta".into()),
                                        data: json!({ "type": "content_block_delta", "index": tool_idx, "delta": { "type": "input_json_delta", "partial_json": args_str } }).to_string(),
                                    });
                                    
                                    results.push(MapperChunk {
                                        event: Some("content_block_stop".into()),
                                        data: json!({ "type": "content_block_stop", "index": tool_idx }).to_string(),
                                    });
                                    *tool_call_index += 1;
                                } else {
                                    let fallback = format!("<tool_call>{}</tool_call>", trim_buf);
                                    results.push(MapperChunk {
                                        event: Some("content_block_delta".into()),
                                        data: json!({ "type": "content_block_delta", "index": tool_idx, "delta": { "type": "text_delta", "text": fallback } }).to_string()
                                    });
                                }
                            }

                            pending_text = pending_text[end_idx + "</tool_call>".len()..].to_string();
                            *in_tool_call = false;
                            tool_call_buffer.clear();
                            
                            // Open the next text block
                            results.push(MapperChunk { event: Some("content_block_start".into()), data: format!(r#"{{"type":"content_block_start","index":{},"content_block":{{"type":"text","text":""}}}}"#, next_text_idx) });
                        } else {
                            tool_call_buffer.push_str(&pending_text);
                            pending_text = String::new();
                        }
                    }
                }
            }
        }
        Ok(results)
    }

    fn extract_workspace(req: &Self::Request) -> Option<String> {
        // Claude Code 在 system prompt 里传入 cwd，通常包含 "cwd" 或 "working directory" 关键词
        let system_text: String = if let Some(sys) = &req.system {
            if let Some(s) = sys.as_str() {
                s.to_string()
            } else if let Some(arr) = sys.as_array() {
                arr.iter()
                    .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                return None;
            }
        } else {
            return None;
        };

        for line in system_text.lines() {
            let lower = line.to_lowercase();
            if lower.contains("cwd") || lower.contains("working directory") || lower.contains("current directory") {
                if let Some(pos) = line.find('/') {
                    let path_candidate = line[pos..].split_whitespace().next()
                        .unwrap_or("")
                        .trim_end_matches(['\'' , '"', '.', ',']);
                    if !path_candidate.is_empty() && std::path::Path::new(path_candidate).is_absolute() {
                        tracing::info!("📁 [AnthropicMapper] 从 system prompt 提取工作区: {}", path_candidate);
                        return Some(path_candidate.to_string());
                    }
                }
            }
        }
        None
    }
}
