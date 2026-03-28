use async_trait::async_trait;
use anyhow::Result;
use serde_json::json;
use crate::mappers::{ProtocolMapper, MapperChunk};
use crate::gemini::{GeminiContentRequest};

pub struct GeminiMapper;

#[async_trait]
impl ProtocolMapper for GeminiMapper {
    type Request = GeminiContentRequest;

    fn get_protocol() -> String {
        "gemini".to_string()
    }

    fn get_model(req: &Self::Request) -> &str {
        // Gemini 原生协议将模型放在 URL path 中，由上层路由注入到请求体
        req.model.as_deref().unwrap_or("gemini-native")
    }

    fn build_prompt(req: &Self::Request) -> Result<crate::mappers::ParsedPrompt> {
        let mut prompt = String::new();
        let mut images = Vec::new();
        let mut media = Vec::new();

        if let Some(tools_wrapper) = &req.tools {
            let mut unified_tools = vec![];
            for tw in tools_wrapper {
                if let Some(decls) = &tw.function_declarations {
                    for fn_decl in decls {
                        unified_tools.push(crate::tools::UnifiedToolDefinition {
                            name: fn_decl.name.clone(),
                            description: fn_decl.description.clone(),
                            parameters: fn_decl.parameters.clone().unwrap_or_else(|| json!({})),
                        });
                    }
                }
            }
            let tool_prompt = crate::tools::build_tool_system_prompt(&unified_tools);
            if !tool_prompt.is_empty() {
                prompt.push_str(&tool_prompt);
                prompt.push_str("\n\n");
                prompt.push_str("IMPORTANT: If you need to use any of the tools above, you MUST output a <tool_call> XML tag containing the tool name and arguments in JSON format. For example:\n<tool_call>{\"name\": \"tool_name\", \"arguments\": {\"arg1\": \"val1\"}}</tool_call>\nAfter outputting the tag, you should stop generating and wait for the result.\n\n");
            }
        }

        if let Some(sys) = &req.system_instruction {
            for p in &sys.parts { if let Some(t) = &p.text { prompt.push_str(t); prompt.push_str("\n\n"); } }
        }
        for c in &req.contents {
            for p in &c.parts { 
                if let Some(t) = &p.text { 
                    prompt.push_str(t); prompt.push('\n'); 
                } else if let Some(inline_data) = &p.inline_data {
                    if let (Some(mime), Some(data)) = (
                        inline_data.get("mimeType").and_then(|v| v.as_str()),
                        inline_data.get("data").and_then(|v| v.as_str())
                    ) {
                        use base64::Engine as _;
                        let decoded = base64::engine::general_purpose::STANDARD.decode(data).unwrap_or_default();

                        images.push(crate::proto::exa::codeium_common_pb::ImageData {
                            base64_data: data.to_string(),
                            mime_type: mime.to_string(),
                            caption: String::new(),
                            uri: String::new(),
                        });
                        media.push(crate::proto::exa::codeium_common_pb::Media {
                            mime_type: mime.to_string(),
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
            let chunk = json!({
                "candidates": [{
                    "index": 0,
                    "content": { "parts": [] },
                    "finishReason": "STOP"
                }],
                "usageMetadata": { "promptTokenCount": 0, "candidatesTokenCount": 0, "totalTokenCount": 0 }
            });
            results.push(MapperChunk { event: None, data: chunk.to_string() });
            return Ok(results);
        }

        match delta {
            crate::mappers::CascadeDelta::Thinking(_) => {
                return Ok(results);
            }
            crate::mappers::CascadeDelta::Text(text) => {
                let mut pending_text = text;
                while !pending_text.is_empty() {
                    if !*in_tool_call {
                        if let Some(start_idx) = pending_text.find("<tool_call>") {
                            let before_text = &pending_text[..start_idx];
                            if !before_text.is_empty() {
                                results.push(MapperChunk { event: None, data: generate_gemini_chunk(model, before_text)? });
                            }
                            *in_tool_call = true;
                            pending_text = pending_text[start_idx + "<tool_call>".len()..].to_string();
                        } else {
                            results.push(MapperChunk { event: None, data: generate_gemini_chunk(model, &pending_text)? });
                            pending_text = String::new();
                        }
                    } else {
                        if let Some(end_idx) = pending_text.find("</tool_call>") {
                            let inner_text = &pending_text[..end_idx];
                            tool_call_buffer.push_str(inner_text);
                            let trim_buf = tool_call_buffer.trim();
                            if !trim_buf.is_empty() {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(trim_buf) {
                                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("unknown_tool").to_string();
                                    let args = v.get("arguments").cloned().unwrap_or_else(|| json!({}));
                                    results.push(MapperChunk { event: None, data: generate_gemini_tool_call_chunk(model, &name, args)? });
                                    *tool_call_index += 1;
                                } else {
                                    let fallback = format!("<tool_call>{}</tool_call>", trim_buf);
                                    results.push(MapperChunk { event: None, data: generate_gemini_chunk(model, &fallback)? });
                                }
                            }
                            tool_call_buffer.clear();
                            *in_tool_call = false;
                            pending_text = pending_text[end_idx + "</tool_call>".len()..].to_string();
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

fn generate_gemini_chunk(_model: &str, content: &str) -> Result<String> {
    let chunk = json!({
        "candidates": [{
            "index": 0,
            "content": {
                "role": "model",
                "parts": [{ "text": content }]
            }
        }]
    });
    Ok(chunk.to_string())
}

fn generate_gemini_tool_call_chunk(_model: &str, name: &str, args: serde_json::Value) -> Result<String> {
    let chunk = json!({
        "candidates": [{
            "index": 0,
            "content": {
                "role": "model",
                "parts": [{
                    "functionCall": {
                        "name": name,
                        "args": args
                    }
                }]
            }
        }]
    });
    Ok(chunk.to_string())
}
