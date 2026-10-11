# gpui-kit 引入评估报告

日期：2026-10-11
状态：评估完成，未做决定

## 背景

目标是引入 [gpui-kit](https://github.com/longbridge/gpui-kit)（Longbridge 的 GPUI 组件库，75+ 组件）。
该库锁定 GPUI 的 crates.io 快照版 `gpui-pre =0.3.8`，而 Fintwind 当前用 git fork。
本报告回答：换成 `gpui-pre =0.3.8` 会引起多大问题。

## 现状

| 项 | 当前值 |
| --- | --- |
| GPUI 来源 | `egoist/zed` fork，`waku-webview` 分支，锁定 commit `f9bad89` |
| fork 内容 | 上游 main + PR #61945（分层场景渲染） |
| 用途 | 让 GPUI 的菜单、tooltip、弹出层画在浏览器 WebView2 之上 |
| 依赖面 | 55 个源文件、292 处 gpui 调用，另有 `gpui_platform`（font-kit）与 `test-support` |

## 关键发现

### 1. PR #61945 已死，不在 gpui-pre 里

- 该 PR 于 2026-09-27 被上游**关闭，未合并**（mergeable_state: dirty）。
  Cargo.toml 注释里"PR 合并后回到上游"的前提已不成立。
- 我下载了 gpui-pre 0.3.8 的源码包（zed@279fe07 快照）并搜索：
  `enable_scene_overlay`、`create_native_surface`、`overlay_scene_start` 均**不存在**。

### 2. gpui-kit 官方承认 Windows 弹出层问题未解决

其 webview 组件文档（`crates/webview/README.md`）原文：

> Windows | WebView2 | Not yet; the WebView covers overlays
> On Windows, use the webview in a separate window or a Popup layer when
> overlapping GPUI content is required.

这正是 Fintwind 用 fork 解决掉的问题。Longbridge 自己的产品线也没有答案。

### 3. 依赖绑定是刚性的

gpui-kit 0.7.1 用 `=0.3.8` 精确锁定快照（历史上有过宽松版本导致应用被
周更快照弄坏的事故，见其 Cargo.toml 注释）。同一个 `gpui` 不能既来自
crates.io 又来自 git——类型互不兼容。不存在"主界面用 gpui-kit、
浏览器表面留在 fork"的混合方案。

## 影响评估

### 编译错误：量大，可修

- 版本跨度：8 月末 main（`f9bad89`）→ 10 月初 main（`279fe07`），约六周上游漂移。
- zed 的 gpui API 每周都有破坏性变更。按 292 处调用的规模，预计几十到一两百处
  编译错误，全部是机械性修复（签名变化、调用方式调整）。
- 连带改动：`gpui_platform` → `gpui-pre-platform`；dev-dependencies 的
  `test-support` feature 仍存在，需确认行为一致。
- 工作量估计：1–3 天的编译修复 + 回归验证。

### 功能回归：不可接受，这是真正的阻断项

- 切换后代码可以编译通过，但浏览器表面上所有 GPUI 弹出层
  （菜单、tooltip、右键菜单、地址栏下拉）会被 WebView2 盖住。
- 这类问题不报错，只在使用中暴露，属于静默的功能丢失。

## 可选路径

| 路径 | 内容 | 代价 | 浏览器表面 |
| --- | --- | --- | --- |
| A. 暂缓 | 保持 fork，等 Windows 弹出层问题有官方答案 | 零 | 完整 |
| B. 切换 + patch | 换 gpui-pre，再把 #61945 移植进 `[patch.crates-io]` 指向的源码 | 最高：修编译错误 + rebase 渲染层补丁 + 长期维护两套结构 | 可保，但补丁要自己扛 |
| C. 切换 + 降级 | 换 gpui-pre，弹出层改走独立窗口或避让布局 | 中：编译修复 + 浏览器交互重设计 | 降级，交互受限 |
| D. 先验证 | 开分支换依赖跑完整编译，拿到确切错误清单再决定 | 半天 | 不变 |

## 建议

1. **短期选 A 或 D。** 编译错误不是问题，弹出层才是。在 Windows 合成问题
   解决前，切换的收益（组件库）抵不过浏览器表面的功能损失。
2. 若决定推进，先做 D 拿到错误清单，再在 B 与 C 之间选。
3. 可到 gpui-kit 仓库确认 #61945 关闭后的后续计划（作者 huacnlee 同时是
   gpui-pre 的发布者，他的去向决定这条路是否还有戏）。
4. 注意：gpui-kit 的 webview 组件强制走 GPUI Fast 后端，明确不支持
   gpui-pre。就算引入 gpui-kit 主库，也不要指望用它的 webview 替代
   Fintwind 自研的 WebView2 组合控制器（Fintwind 需要 composition
   controller，这也是当初弃用 wry 的原因）。

## 证据来源

- PR #61945 状态：<https://github.com/zed-industries/zed/pull/61945>
  （`"merged": false, "closed_at": "2026-09-27T11:56:05Z"`）
- gpui-pre 0.3.8：<https://crates.io/crates/gpui-pre>（zed@279fe07 快照，
  2026-10-05 发布）；本地解包搜索确认无 overlay API
- gpui-kit 锁定方式：<https://github.com/longbridge/gpui-kit> 仓库根
  Cargo.toml（`gpui = { package = "gpui-pre", version = "=0.3.8" }`）
- gpui-kit webview 限制：同仓库 `crates/webview/README.md`
- 本仓库依赖面统计：`rg "use gpui|gpui::" src crates -g "*.rs"`，
  55 文件 / 292 行（2026-10-11）
