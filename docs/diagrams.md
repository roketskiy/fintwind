# Mermaid 图表说明

Go 桌面端使用 limu 与其 MyGo 适配器渲染对话中的 Mermaid 图表，版本固定在
[go.mod](../go.mod)。limu 使用 MIT 许可，声明见 [limu.txt](licenses/limu.txt)。

图表排版在后台执行，界面只读取缓存。源码超过 32 KiB、缓存达到上限、尚未完成排版
或排版失败时，界面显示代码内容。渲染不允许外部图片和链接。
实现由 [cmd/app](../cmd/app) 的 Markdown 渲染模块负责。

旧 Rust 版本使用 Ondine，与 Go 版的渲染能力和校验工具不同。
相关源码位于 [legacy/rust](../legacy/rust/README.md)，不参与当前构建。
旧版校验工具及偏好约定见 [mermaid-prompt.md](mermaid-prompt.md)。
