# Feature: 深度多模态增强、思考链 (CoT) 提取与中文推理引导

## 变更描述 (Description)

本 PR 是对 Antigravity-Tools-LS 的重大功能升级。除了完善原有的多模态图像对齐外，核心引入了对 **Cascade 模型思考链 (Thinking Chain)** 的深度解析与展示优化，极大提升了推理类模型（如 Gemini 2.0 Thinking, Claude 3.7 Sonnet）的使用体验。

核心改进如下：

### 1. 思考链 (CoT) 深度集成
- **协议解析**：通过对 gRPC 响应结构的逆向，成功提取了 `CortexStepPlannerResponse` 的 **字段 3 (thinking)**。
- **分流传输**：引入 `CascadeDelta` 枚举，在流转码逻辑中显式区分“思考过程”与“最终回复”，确保数据流不混淆。
- **UI 适配 (OpenAI Standard)**：将思考内容映射至 **`reasoning_content`** 字段。这使得支持该协议的前端（如 RikkaHub, LobeChat）能够以原生的“可折叠卡片”形式渲染思考链，而正文保持纯净。

### 2. 中文推理引导 (Chinese Reasoning Enforcement)
- **指令注入**：在会话初始化阶段，自动向模型注入引导指令：`"Please reason in Chinese. (请务必使用中文进行思考)"`。
- **体验提升**：解决了推理模型默认使用英文思考的问题，让推理过程对中文用户更加友好直观。

### 3. 多模态图像传输增强 (Dual-Field Support)
- **双字段冗余**：同时支持 `ImageData` (字段 6) 与 `Media` (字段 14) 传输逻辑。
- **全模型对齐**：修复了 Gemini Flash 等模型由于不解析 Base64 导致的“看不见图”问题。

### 4. 架构优化与 Bug 修复
- **异步链路修复**：重构了 `engine.rs` 中的转码循环，修复了异步闭包导致的编译与运行期作用域问题。
- **会话稳定性**：加固了 `CascadeClient` 的调用链路，修复了特定场景下的 403 权限回归。

---

## 解决的 Bug/Feature

- [x] **思考链显示**：解决推理模型思考过程丢失或混入正文的问题。
- [x] **思考语言锁定**：强制模型在推理阶段使用中文。
- [x] **图像识别修复**：提升了多模态模型对图片的识别成功率。
- [x] **全协议支持**：完美支持 RikkaHub、NextChat 等客户端的图片上传与思考折叠。

---

## 测试步骤 (How to test)

1.  编译并运行最新版 `target/release/cli-server`。
2.  使用支持推理的模型（如 `gemini-2.0-flash-thinking-exp`）。
3.  上传一张包含逻辑问题的图片，并询问：“请分析图片中的问题”。
4.  **预期结果**：
    - 前端出现折叠的“思考中...”区域。
    - 思考内容为**中文**。
    - 图片被准确识别并给出最终答复。

---

## 影响范围与代码变更 (Diff Patch)

相关的**完整 Git Diff 代码补丁 (对比初始版本)**：[`feature_image_support.patch`](file:///Users/buildin1/Desktop/package/github/Antigravity-Tools-LS/pull_request/feature_image_support.patch)。

可以使用以下命令快速合并：
```bash
git apply pull_request/feature_image_support.patch
```
