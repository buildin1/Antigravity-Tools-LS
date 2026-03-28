use async_trait::async_trait;
use serde::de::DeserializeOwned;
use anyhow::Result;

pub mod openai;
pub mod anthropic;
pub mod gemini;
pub mod engine;

use crate::proto::exa::codeium_common_pb::{ImageData, Media};

/// 映射后的增量响应块
#[derive(Debug, Clone)]
pub struct MapperChunk {
    pub event: Option<String>,
    pub data: String,
}

/// 底层 Cascade 返回的聚合增量数据类型
/// 考虑到有的模型同时输出思考内容与最终文本，我们需要在流中区分它们。
#[derive(Debug, Clone)]
pub enum CascadeDelta {
    Text(String),
    Thinking(String),
}

/// 解析后的请求信息（含图片）
#[derive(Debug, Clone)]
pub struct ParsedPrompt {
    pub text: String,
    pub images: Vec<ImageData>,
    pub media: Vec<Media>,
}

/// 流式传输的元数据，在流结束时发送
#[derive(Debug, Clone, Default)]
pub struct StreamMetadata {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub error: Option<String>,
}

/// 协议转换器 Trait，用于将各种 API 协议映射到底层的 Cascade 转码逻辑
#[async_trait]
pub trait ProtocolMapper: Send + Sync + 'static {
    /// 请求类型
    type Request: DeserializeOwned + Send + Sync + 'static;
    
    /// 获取协议标识符
    fn get_protocol() -> String;

    /// 获取模型名称
    fn get_model(req: &Self::Request) -> &str;

    /// 将原始请求转换为底层的 Prompt 与图片数据
    fn build_prompt(req: &Self::Request) -> Result<ParsedPrompt>;

    /// 处理底层的增量数据，转换为特定协议的流式响应格式（Chunk）
    async fn map_delta(
        model: &str,
        delta: CascadeDelta, // 改为使用 CascadeDelta
        is_final: bool,
        tool_call_buffer: &mut String,
        in_tool_call: &mut bool,
        tool_call_index: &mut u32,
    ) -> Result<Vec<MapperChunk>>;

    /// 返回初始帧（可选）
    fn initial_chunks() -> Vec<MapperChunk> {
        vec![]
    }

    /// 提取请求中的工作区目录（可选，默认返回 None）
    fn extract_workspace(req: &Self::Request) -> Option<String> {
        let _ = req;
        None
    }
}
