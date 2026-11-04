# Mermaid 图表说明

对话中的 `mermaid` 代码围栏绘制为原生 SVG。编译器是
[Ondine](https://github.com/wevm/ondine)（crate `ondine`，`compiler` 包，MIT），
版本钉在 `crates/fintwind-diagram/Cargo.toml`。Ondine 是纯 Rust：无 DOM、
无 JavaScript、无网络。模块把它的主题 SVG 转为原生 SVG 渲染器（resvg/usvg）
可直接绘制的文档，无需 CSS 自定义属性。许可见 `licenses/ondine-MIT.txt`。

编译器放在独立 crate（`crates/fintwind-diagram`），应用渲染（`src/md/diagram.rs`）
与 daemon 校验端点（`crates/fintwind-core/src/mermaid_check.rs`）调用同一编译，
「校验通过」与「能画出来」是同一事实。

## 支持与限制

- 支持 Ondine 覆盖的全部图表类型，常用流程图、时序图、类图、状态图、实体关系图、
  饼图与甘特图。
- 图表跟随主题、字号与高 DPI 缩放，支持切换源码、键盘滚动与复制；编译失败保留
  源码并显示具体原因与行列位置。
- 单源上限 32 KiB，输出上限 1 MiB，流程图边上限 512 条；宽高须有限、为正且不
  超过 100000。甘特图日期以源码为准，同一源码在主题/DPI 切换后日期不变。
- 不支持 HTML 标签、图片、链接、自定义 CSS 与指令；作者指定的显式颜色保留，
  外部引用与脚本会被拒绝。
- 五色主题驱动 Ondine 的变量别名；未知变量用其 fallback，无 fallback 时回退
  前景色，整图不因作者笔误失败。
- 宽图绘制变窄时，东亚文字优先使用屏幕 UI 字体（如中文 Microsoft YaHei UI），
  避免笔画糊掉；等宽根字体保持不变。

## 校验工具

模型写图时可用 `fintwind_mermaid_check` 工具做编译校验。校验偏好版本规则见
`mermaid-prompt.md`。

升级 Ondine 用 `bun scripts/update-ondine.ts`，渲染差异需有意复核后再提交。
