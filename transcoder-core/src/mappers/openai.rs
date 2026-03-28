use async_trait::async_trait;
use anyhow::Result;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::mappers::{ProtocolMapper, MapperChunk};
use crate::openai::{OpenAIChatRequest};

pub struct OpenAiMapper;

#[async_trait]
impl ProtocolMapper for OpenAiMapper {
    type Request = OpenAIChatRequest;

    fn get_protocol() -> String {
        "openai".to_string()
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
                name: t.function.name.clone(),
                description: t.function.description.clone(),
                parameters: t.function.parameters.clone().unwrap_or_else(|| json!({})),
            }).collect::<Vec<_>>();
            
            let tool_prompt = crate::tools::build_tool_system_prompt(&unified_tools);
            if !tool_prompt.is_empty() {
                prompt.push_str(&tool_prompt);
                prompt.push_str("\n\n");
                prompt.push_str("IMPORTANT: If you need to use any of the tools above, you MUST output a <tool_call> XML tag containing the tool name and arguments in JSON format. For example:\n<tool_call>{\"name\": \"tool_name\", \"arguments\": {\"arg1\": \"val1\"}}</tool_call>\nAfter outputting the tag, you should stop generating and wait for the result.\n\n");
            }
        }

        for msg in &req.messages {
            if let Some(content) = &msg.content {
                if let Some(text) = content.as_str() {
                    prompt.push_str(text);
                    prompt.push('\n');
                } else if content.is_array() {
                    if let Some(arr) = content.as_array() {
                        for item in arr {
                            if let Some(type_str) = item.get("type").and_then(|t| t.as_str()) {
                                if type_str == "text" {
                                    if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                                        prompt.push_str(t);
                                        prompt.push('\n');
                                    }
                                } else if type_str == "image_url" {
                                    if let Some(url) = item.get("image_url").and_then(|u| u.get("url")).and_then(|u| u.as_str()) {
                                        if url.starts_with("data:") {
                                            let parts: Vec<&str> = url.splitn(2, ',').collect();
                                            if parts.len() == 2 {
                                                let meta = parts[0];
                                                let base64_data = parts[1].to_string();
                                                let mime_type = if meta.len() > 5 {
                                                    meta[5..].split(';').next().unwrap_or("image/jpeg").to_string()
                                                } else {
                                                    "image/jpeg".to_string()
                                                };

                                                use base64::Engine as _;
                                                let decoded = base64::engine::general_purpose::STANDARD.decode(&base64_data).unwrap_or_default();

                                                images.push(crate::proto::exa::codeium_common_pb::ImageData {
                                                    base64_data: base64_data.clone(),
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
                                        } else {
                                            images.push(crate::proto::exa::codeium_common_pb::ImageData {
                                                base64_data: String::new(),
                                                mime_type: "image/jpeg".to_string(),
                                                caption: String::new(),
                                                uri: url.to_string(),
                                            });
                                            media.push(crate::proto::exa::codeium_common_pb::Media {
                                                mime_type: "image/jpeg".to_string(),
                                                description: String::new(),
                                                uri: url.to_string(),
                                                thumbnail: vec![],
                                                duration_seconds: 0.0,
                                                payload: None,
                                            });
                                        }
                                    }
                                }
                            } else {
                                // Fallback
                                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                                    prompt.push_str(t);
                                    prompt.push('\n');
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(crate::mappers::ParsedPrompt { text: prompt, images, media })
    }

    async fn map_delta(
        model: &str,
        delta: crate::mappers::CascadeDelta,
        is_final: bool,
        tool_call_buffer: &mut String,
        in_tool_call: &mut bool,
        tool_call_index: &mut u32,
    ) -> Result<Vec<MapperChunk>> {
        let mut results = vec![];
        
        if is_final {
            results.push(MapperChunk { event: None, data: generate_chunk(model, "", None, true)? });
            return Ok(results);
        }

        match delta {
            crate::mappers::CascadeDelta::Thinking(think_text) => {
                // 🚀 核心核心适配：将思考内容映射到 reasoning_content 字段
                if !think_text.is_empty() {
                    results.push(MapperChunk { event: None, data: generate_chunk(model, "", Some(&think_text), false)? });
                }
            }
            crate::mappers::CascadeDelta::Text(text) => {
                if text.is_empty() { return Ok(results); }

                let mut pending_text = text;
                while !pending_text.is_empty() {
                    if !*in_tool_call {
                        if let Some(start_pos) = pending_text.find("<tool_call>") {
                            *in_tool_call = true;
                            let prefix = &pending_text[..start_pos];
                            if !prefix.is_empty() {
                                results.push(MapperChunk { event: None, data: generate_chunk(model, prefix, None, false)? });
                            }
                            pending_text = pending_text[start_pos + "<tool_call>".len()..].to_string();
                        } else {
                            results.push(MapperChunk { event: None, data: generate_chunk(model, &pending_text, None, false)? });
                            pending_text = String::new();
                        }
                    } else {
                        if let Some(end_pos) = pending_text.find("</tool_call>") {
                            let inner_text = &pending_text[..end_pos];
                            tool_call_buffer.push_str(inner_text);
                            let trim_buf = tool_call_buffer.trim();
                            if !trim_buf.is_empty() {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(trim_buf) {
                                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("unknown_tool").to_string();
                                    let args = v.get("arguments").map(|a| if let Some(s) = a.as_str() { s.to_string() } else { a.to_string() }).unwrap_or_else(|| "{}".to_string());
                                    results.push(MapperChunk { event: None, data: generate_tool_call_chunk(model, &name, &args, *tool_call_index)? });
                                    *tool_call_index += 1;
                                } else {
                                    let fallback = format!("<tool_call>{}</tool_call>", trim_buf);
                                    results.push(MapperChunk { event: None, data: generate_chunk(model, &fallback, None, false)? });
                                }
                            }
                            tool_call_buffer.clear();
                            *in_tool_call = false;
                            pending_text = pending_text[end_pos + "</tool_call>".len()..].to_string();
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
}

fn generate_chunk(model: &str, content: &str, reasoning_content: Option<&str>, is_final: bool) -> Result<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    
    let delta = if is_final {
        json!({})
    } else if let Some(reasoning) = reasoning_content {
        // 🚀 OpenAI 官方标准的 reasoning_content 字段
        json!({ "reasoning_content": reasoning })
    } else {
        json!({ "content": content })
    };

    let chunk = json!({
        "id": format!("chatcmpl-cascade-{}", uuid::Uuid::new_v4()),
        "object": "chat.completion.chunk",
        "created": now,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": if is_final { json!("stop") } else { serde_json::Value::Null }
        }]
    });
    Ok(chunk.to_string())
}

fn generate_tool_call_chunk(model: &str, name: &str, args: &str, tool_call_index: u32) -> Result<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let chunk = json!({
        "id": format!("chatcmpl-cascade-{}", uuid::Uuid::new_v4()),
        "object": "chat.completion.chunk",
        "created": now,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": tool_call_index,
                    "id": format!("call_{}_{}", uuid::Uuid::new_v4().to_string().replace("-", ""), tool_call_index),
                    "type": "function",
                    "function": { "name": name, "arguments": args }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    Ok(chunk.to_string())
}
