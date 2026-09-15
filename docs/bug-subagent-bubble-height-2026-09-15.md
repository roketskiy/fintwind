# Bug 调查报告：子代理面板的用户气泡随面板变宽而变矮

日期：2026-09-15　|　现场：`dev` 运行中的构建（截图 21:03）　|　状态：已定位根因，未修复

## 现象

右侧面板打开一个 subagent 会话（`BackgroundWorkKind::Subagent`）时，该会话里**用户消息气泡的高度与自身内容不一致**：面板越宽，气泡越矮，末尾若干行文本溢出到气泡背景之外，并与紧随其后的活动行、助手消息重叠。助手消息不受影响。

窄面板下一切正常，因此该缺陷只在面板被拉宽后出现。

## 结论（TL;DR）

气泡同时设置了 `w_full()`（`width: 100%`）和 `max_w(540)`，而它的父容器是**列方向** flex 容器。在 Taffy 里，列容器的子项 `width:100%` 会被解析成一个确定宽度（= 面板内容宽），这个宽度**不经过 `max-width` 夹取**就被当作已知宽度传给文本测量；而最终布局用的是夹取后的 540。GPUI 的文本测量以「已知宽度优先、可用空间兜底」决定折行宽度，于是：

- **框高**按「面板内容宽」折出的行数算；
- **画面上的文本**按 `540 - 24 = 516` 折行绘制。

面板越宽，两个宽度差越大，框就越矮，而文本行数不变 → 文本溢出。面板宽度 ≤ 572 逻辑像素时两者相等，所以窄面板完全正常。

## 代码链路

| 位置 | 作用 |
| --- | --- |
| `src/app/background_work.rs:1837` | 渲染子代理会话消息时传 `user_message_fill_width: true` |
| `src/app/background_work.rs:1826-1851` | 子代理转写走 `render_message`，`MarkdownMetrics::user_message()` |
| `src/app/background_work.rs:1808` | 行容器 `div().w_full().min_w_0().flex().flex_col().gap(8)` |
| `src/app/background_work.rs:1580-1595` | 外层滚动区 `overflow_y_scroll().px(16).py(12)` |
| `src/app/components.rs:699-711` | 用户气泡本体：`max_w(540).min_w_0().w_full().rounded(12).px(12).py(8)` |
| `src/app/transcript_view.rs:1308` | 主会话传 `user_message_fill_width: false`（宽度 auto，不触发本缺陷） |
| `src/md/render.rs:84-90, 110-112` | `USER_MESSAGE` = 14.0 / 20.0 |

依赖侧（GPUI 固定 `egoist/zed` `f9bad89`，taffy 0.12.2）：

| 位置 | 行为 |
| --- | --- |
| `crates/gpui/src/elements/text.rs:650-655` | `wrap_width = known_dimensions.width.or(available_space.width)` — **已知宽度优先** |
| `crates/gpui/src/elements/text.rs:687-694` | 测量缓存按 `wrap_width` 命中，宽度一变则重新折行 |
| `taffy src/compute/flexbox.rs:684-696` | `cross_axis_available_space` 用 `child_max_cross` 夹到 540 |
| `taffy src/compute/flexbox.rs:699-712` | `child_known_dimensions = child.size.with_main(dir, None)` — `width:100%` 的确定宽度**未做 max 夹取** |
| `taffy src/compute/flexbox.rs:1386-1426` | 列容器用 `child_available_cross`（已夹到 540）求子项高度 |
| `taffy src/compute/flexbox.rs:1936-1943` | 最终布局用 `item.target_size`（宽度已夹到 540）重排子树 → 文本按 516 重折并覆盖测量缓存 |

用一句话概括这条链：列容器的 `width:100%` 子项，「用于测量高度」的宽度和「用于绘制」的宽度不是同一个值，而 `max_w` 只作用于后者。

## 截图像素证据

三张截图来自同一会话，缩放 1.25×（1 逻辑像素 = 1.25 物理像素，行距 20 逻辑 = 25 物理，与 `USER_MESSAGE` 一致）。

| 截图 | 面板内容宽 | 气泡背景 | 文本最宽 | 结果 |
| --- | --- | --- | --- | --- |
| `21-03-27.png` / `21-03-32.png`（1433×916） | 532 逻辑 | 右对齐，`min(532,540)=532` | 约 508 逻辑 | 正常：气泡底 769 物理，末行文本底 756，后续活动行 829 |
| `21-03-14.png`（979×852） | 749 逻辑 | 675 物理 = **540 逻辑**（`max_w` 生效） | 641 物理 = **516 逻辑** | 异常：气泡底 y=512，而文本一直画到 y=726 |

宽面板截图的判定要点：

- 气泡背景 x 281..955（675 物理 = 540 逻辑），右侧留白 20 物理 = 16 逻辑，正好是滚动区的 `px(16)` —— 证明绘制宽度确实被 `max_w(540)` 夹住。
- 文本左缘 297 物理 = 气泡左缘 + 12 逻辑（`px(12)`），文本最宽 641 物理 = **516 逻辑 = 540 - 24**，即文本按夹取后的宽度折行。
- 气泡底边 y=512，而属于同一条消息的末段「已知约束：仓库用 eprintln! ……轻微风格问题简单列出即可。」（4 行）整段画在 512 之下，并与 `我先锁定基点并获取未提交改动的 diff`（y≈572）等行重叠。
- 反推的框高对应「按面板内容宽 749 折行」的行数，而实际文本按 516 折行、行数约多出 7~9 行（≈ 140~170 逻辑像素），与测得的溢出量级一致。

## 触发阈值

`面板内容宽 - 24 > 540`，即 **面板宽度 > 572 逻辑像素**（窗口宽 - 侧栏 - 主区域 - 面板内边距）才开始出问题。内容宽 ≤ 540 时 `min(内容宽, 540) == 内容宽`，测量与绘制宽度一致，因此完全正常 —— 这正是「窄面板没事、拉宽才坏、越宽越扁」的来源。

## 影响

- 视觉：消息末尾文本溢出气泡，与后续行重叠，气泡圆角与悬停底色范围也偏小。
- 滚动：滚动内容高度按错误的框高累加，溢出到父级高度之外的部分可能滚不到底。
- 选择：`md::render::selection` 的选取几何基于实际文本布局，与气泡背景不一致；跨行复制体验受损。

## 修复方向（未实施）

根因是「列容器里 `width:100%` + `max-width` 的交叉轴夹取没有进入子内容的已知宽度」。让宽度变成**主轴**即可：

- 在气泡外套一层 `div().w_full().flex().flex_row().justify_end()`，气泡本身保留 `max_w(540)`（去掉 `w_full()`）。
  这样 Taffy 会先在 `resolve_flexible_lengths` 里把宽度夹到 540，再由 `determine_hypothetical_cross_size`（`is_row` 分支用 `child.target_size.width`）以 540 去测量高度，测量宽度与绘制宽度一致。
- 备选：完全不用 `w_full()` + `max_w` 组合，改用不依赖交叉轴夹取的显式宽度写法。

两种都只影响子代理面板（`user_message_fill_width: true`）的路径；主会话传 `false`，行为不变。

## 复现与验证

1. 打开子代理会话面板，把面板拖到 > 572 逻辑像素（内容宽 > 540）。
2. 观察用户气泡：文本末段溢出到气泡外并与后续行重叠；把面板收窄回 540 以下即恢复正常。
3. 若需在测试中固化，可用 `VisualTestContext::draw` 以两种宽度各绘制一次气泡，比较文本框（`div` 高度）是否一致。
