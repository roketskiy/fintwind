# gpui-kit 增量迁移方案

日期：2026-10-11
状态：源码核实完成；未做兼容编译，未修改产品代码或依赖，未运行应用。

## 1. 工作区与研究范围

- 工作树：`E:/work/Opencode-Desktop/fintwind-gpui-kit-migration`。
- 分支：`research/gpui-kit-migration`。
- 基点：`b26d6d1d5cce9d6f52eb015380b7cbaf393c4fcb`。
- 原工作区保持不动；原评估文档已复制，SHA256 为
  `83c227364431371d5edf5337e5849f25cd06b2b0c32d9f3cff7e8d38a08ed1a2`。
- 本文补充 [原评估](gpui-kit-migration-assessment.md)，不覆盖用户材料。
- 目标版本为 `gpui-kit 0.7.1` / `gpui-pre 0.3.8`。不能把后续 main 的
  Fast 后端说明或 API 当作该版本已经具备的能力。

**结论：把底层依赖兼容与组件替换分开。先验证依赖和原生叠层，再以设置页
试点；不同时重写浏览器、输入器、对话记录或终端。**

## 2. 已核实的阻断项与勘误

### 2.1 双包共存不等于类型互通

Cargo 可以同时解析不同来源的 GPUI 包；但应用与组件库交接的
`Window`、`App`、`Entity` 和元素 trait 必须属于同一套 GPUI 类型。
gpui-kit 的 facade 本身重导出底层 GPUI，因此依赖统一后，不必为接入组件库
把全项目 `gpui::` 导入改成 `gpui_kit::`。

当前 fork 的包名为 `gpui`、版本为 `0.2.2`，而库要求的包名为 `gpui-pre`、
版本为 `=0.3.8`。不能直接用 `[patch.crates-io]` 将后者指向前者并期待 Cargo
忽略包名和版本约束。依据：本项目 `Cargo.lock:2790-2792`、fork manifest、
gpui-kit 0.7.1 workspace manifest。

### 2.2 浏览器依赖的不只是弹出层开关

- `src/app.rs:2907` 调用 `enable_scene_overlay()`。
- `src/browser.rs:1468` 调用 `create_native_surface()`。
- `src/browser.rs:235-237` 使用 `PlatformNativeSurface`。
- `src/browser.rs:963-967` 将 surface 的 `IUnknown` 交给
  `SetRootVisualTarget`；当前桥接依赖 `windows 0.61` 的具体 Rust 类型。

已搜索实际发布的 `gpui-pre 0.3.8/src`，上述 overlay/native-surface API
不存在。直接换快照会先产生编译错误；不能只把开关设为 false 就保留现有宿主。

虽然 `src/app/right_panel.rs:1681` 有遮挡降级分支，但
`src/browser.rs:1882` 的 `request_snapshot` 是空实现。该分支不是已完成的
截图退路。丢失 native surface 后还必须重新实现宿主，不能将路线 C 描述为
已有降级能力的简单启用。

### 2.3 Windows 渲染器仍是独立 crate

已下载并读取三个 **实际发布包**，不仅检查依赖表：

- `gpui-pre-platform 0.3.8/src/gpui_platform.rs:73-78` 在 Windows 下创建
  `gpui_windows::WindowsPlatform`。
- `gpui-pre-windows 0.3.8/src/gpui_windows.rs:8-10` 声明 DirectX 渲染模块。
- `gpui-pre-windows 0.3.8/src/directx_renderer.rs` 包含 `DirectXRenderer`。
- PR #61945 的 diff 落点包含 `gpui/src/{platform,scene,window}.rs` 和
  `gpui_windows/src/{directx_renderer,platform,window}.rs`；没有要求修改
  `gpui_platform` 的启动入口。

因此，不能从 `windows` feature 变多推断“后端已内联到 GPUI 主库”。
Windows 移植的明确落点是 **GPUI 核心 + Windows 后端**；
`gpui-pre-platform`、宏与其他包要检查来源、版本和 feature，但只有实际修改
其源码或包配置时才需要追加 patch。不能将“所有进图包都必须 patch”作为规则。

`windows 0.61` 到 `0.62` 的交接风险仍成立。具体是否统一版本或增加桥接，
要按 native-surface 实现验证，不能只认为 COM ABI 相同就保证 Any downcast 成功。

### 2.4 工期与错误数量尚未测量

原报告的“几十至一两百处错误”和“1–3 天”不是编译结果。
子代理对“旧 fork 回移一定更贵”的判断同样未经过编译验证。
本次没有取得任一路线的真实错误清单，不将这些估计作为路线选择的硬依据。

PR #61945 关闭未合并的状态已复核；不能再以“等该 PR 合并”作为回归上游的计划。

## 3. 依赖路线

| 路线 | 方式 | 主要风险 | 判断 |
| --- | --- | --- | --- |
| B：新快照 + 原生叠层补丁 | 应用与组件库统一到 0.3.8，移植核心接口和 Windows 渲染补丁 | 应用 API 漂移、渲染正确性、COM 类型交接 | 值得优先做源码落点和编译探针；尚未证明可交付 |
| E：组件库适配当前 fork | fork gpui-kit workspace，将其 GPUI 家族依赖统一到当前固定 revision | 库源码回移、feature 差异、长期跟进成本 | 机制可行；作为比较路线，不先宣布便宜或不可行 |
| A：保持现状 | 不引入库 | 继续维护自研通用组件 | 探针失败或成本过高时的正常退出方式 |
| C：降级浏览器 | 改宿主或避让布局，不保留当前合成能力 | 功能损失与浏览器重设计 | 不推荐，需用户明确接受产品降级 |

### E 不一定需要三个 wrapper

可以直接修改 gpui-kit fork 的 workspace dependencies，并让其 path 成员
`gpui-kit`、`gpui-base`、`gpui-component` 等沿用该统一配置。
这与“保留已发布的 gpui-pre 依赖、用同名 wrapper patch”是两种办法；后者
不是唯一机制，也不能因此给前者预先增加三个 wrapper 的维护成本。

核实范围必须包含库直接使用的 `gpui_macros`、`sum-tree` 等依赖；只改
`gpui` 和 `gpui_platform` 两行不等于整图已统一。应用与库的 git URL、
revision/source 配置也要一致，不能只比较 resolved commit。

**下一步不是直接全量迁移，而是 M0。优先核对 B 的完整补丁落点，并以 E 的
实际编译清单作比较；不能只改版本号后就开始替换控件。**

## 4. 迁移边界

| 范围 | 建议 | 依据 |
| --- | --- | --- |
| 设置页普通按钮、开关 | 先试点库组件，继续调用现有业务 setter | `src/app/settings.rs:540-549,584-593` |
| 外观页八个选择器 | 适配受控选择与保留状态；配色/字体行保留自定义呈现 | `src/app/settings.rs:659-925` |
| 普通表单/搜索输入 | 后续迁移，持有 InputState，重接提交、清空和导航动作 | `src/ui/text_field.rs`、`src/app/settings.rs:334-366` |
| Tooltip | 先局部采用，不优先改全局共享 helper | `src/ui/tooltip.rs`；库同时存在原生 tooltip 视图与 Root 管理的 tooltip 行为 |
| 全局菜单 | 后置，不能只替换外观 | `src/ui/menu.rs:57-79` 的动作注册顺序与输入焦点耦合 |
| ComposerInput / composer | 第一轮保留 | 编辑、自动补全、菜单焦点和流式控制耦合 |
| transcript / Markdown 虚拟化 | 第一轮保留 | `docs/performance.md:99-124` 的虚拟化和缓存契约 |
| scrollbar / motion | 第一轮保留 | `docs/performance.md:53-95` 的提交、pulse、hold/fade 节奏 |
| browser / terminal | 保留宿主和行为实现，不用库的 webview 替代 | 原生输入、合成、权限与性能契约 |
| provider/model 图标 | 保留业务映射，必要时适配输出 | `src/ui/mod.rs` |

## 5. 分阶段实施与准入条件

### M0：兼容性与补丁探针，不改产品 UI

1. 固定本次研究的源版本和 source 配置；避免更新锁文件时意外跟随 fork 分支。
2. 对 B 列出核心/Windows 补丁文件、trait 默认实现、surface 所有权和 COM 交接。
3. 对 E 统一库 workspace 的相关依赖，检查 feature 后再做库编译；不引入
   第二套与应用交接的 GPUI 类型。
4. 正常构建和 test-support 检查分开，不能用开启 test-support 的二进制衡量性能。
5. 输出 dependency graph、feature 清单、完整编译日志和按包分类的错误报告。
   没有实测前不报精确工期，也不靠删功能清除编译错误。

产物：两个路线的可复核结果或明确阻断原因。**M0 编译探针尚未执行。**

### M1：启动与 Root 集成

- 初始化库后再构造组件，并保持现有 `input::init -> menu::init` 的相对顺序。
- 每个使用库窗口设施的窗口挂一个 Base Root，不在每页再建 Root。
- 保存实际 `Entity<Fintwind>` / 弱句柄；调整 `src/lib.rs:383-395` 中目前依赖
  `WindowHandle<Fintwind>` 的通知回调，不假设新窗口根仍是 Fintwind。
- 保留现有 Application、字体、系统设置和平台初始化，合并资产源，不整体替换。
- 统一顶层 Escape、焦点恢复与弹层优先级。旧提交弹窗优先级为 4，图片预览为
  5；库 Dialog 从 10 起、Popup 为 100。不能仅因各自系统内部正确就假设混用正确。
- 核对 native menu 的 Copy/Undo 等动作与新 Input 的动作类型，必要时桥接。

特别注意：库 `base/src/popup.rs` 与 `base/src/dialog.rs` 使用 deferred 绘制；
`base/src/sheet.rs` 没有同样的包装。保留 fork 或移植补丁都不能自动保证
**所有**库窗口层位于 WebView2 上方；Sheet、通知、managed tooltip 等须逐类验证。

准入：真实 WebView2 上的新弹层可见、可命中，关闭恢复焦点，后台页面不会误收输入。
现有 browser PoC 没有挂库 Root，也没有开启 scene overlay，不能单独证明此条件。

### M2：主题、字号和资产桥

- Fintwind 设置仍是唯一来源；将已解析的产品主题单向投影到库 Theme。
- 同步所有配色方案、系统明暗切换、UI/代码字体以及分别设置的字号。
- 库 `font_size -> window.set_rem_size` 控制 rem 几何；当前
  `src/theme.rs:84-91` 用 `ui_px/code_px`。两者要映射，不能让一半界面忽略字号设置。
- 仅在启动、用户修改或系统设置变化时同步主题；不要在 render 中调用会刷新
  全部窗口的 Theme 更新方法。
- 新组件的测量缓存要随字体、字号、宽度和 rem 变化失效。

准入：新旧控件同步响应设置，无重复缩放，字体/配色切换不会留下旧布局。

### M3：设置页试点

先迁移 General 的两个开关和 Appearance 的选择器；保持页面布局、翻译、
持久化和业务操作流程。浏览器工具开关必须继续调用已有 setter，不能只改 bool
而绕过权限撤销。选择器状态必须在 render 之外创建，并随外部设置变化同步。

搜索框可作为下一小步，不和全部选择器同时替换；需保留清空、侧栏过滤、
方向键导航与焦点行为。也不为采用 Select 而丢掉配色预览或字体行的自定义内容。

准入：点击与键盘等价；选中状态稳定；Escape 关闭后焦点恢复；重启设置仍存在；
中英文、字体和字号变化可用。保存/更新事件不得因状态回写触发两次。

### M4：普通表单推广

M3 验收后逐页考虑 providers/MCP 等表单。每批都保留独立退出点，先不触及
composer、transcript、terminal 和 browser 输入链。删除自研原语前确认没有
剩余调用方，不把“接入库”解释为必须移除所有自研实现。

## 6. 验证要求与退出条件

- 使用现有整体黑盒/E2E 的真实行为证据；不新增单元测试或源码变更探测测试。
- 运行前确认独立数据目录、环境白名单和子孙进程容纳边界；现有 runner 的存在
  不等于已经满足当前安全规则。不能停止安装版或其他会话的进程。
- 原生适配器 E2E 与完整窗口 Root/弹层验收分开记录；真实模型调用需单独授权。
- 输出日志、JSON 报告、源码/二进制哈希及可重复步骤。视觉测试须另行授权，
  本轮没有截图或视觉验收，也没有启动 watcher。
- 进入流式路径前按 `docs/performance.md:126-149` 测量提交、pane 渲染和 pulse
  计数。编译成功不证明性能没有回退。
- 若任一路线要求浏览器降级、破坏输入或大面积回移组件库，停止扩展并报告；
  不为了完成迁移而扩大范围。

## 7. 证据与可复核材料

- [gpui-kit v0.7.1 workspace manifest](https://github.com/longbridge/gpui-kit/blob/v0.7.1/Cargo.toml)
- [kit facade 与窗口入口](https://github.com/longbridge/gpui-kit/blob/v0.7.1/crates/kit/src/lib.rs)
- [Base Root](https://github.com/longbridge/gpui-kit/blob/v0.7.1/crates/base/src/root.rs)
- [Component Root plugin](https://github.com/longbridge/gpui-kit/blob/v0.7.1/crates/component/src/root.rs)
- [Popup](https://github.com/longbridge/gpui-kit/blob/v0.7.1/crates/base/src/popup.rs)、
  [Dialog](https://github.com/longbridge/gpui-kit/blob/v0.7.1/crates/base/src/dialog.rs)、
  [Sheet](https://github.com/longbridge/gpui-kit/blob/v0.7.1/crates/base/src/sheet.rs)
- [Tooltip 两种接入方式](https://github.com/longbridge/gpui-kit/blob/v0.7.1/crates/component/src/tooltip.rs)
- [字体与 rem](https://github.com/longbridge/gpui-kit/blob/v0.7.1/website/docs/fonts.md)
- [PR #61945](https://github.com/zed-industries/zed/pull/61945) 与
  [完整 diff](https://patch-diff.githubusercontent.com/raw/zed-industries/zed/pull/61945.diff)
- 当前 fork：[Cargo.toml](https://github.com/egoist/zed/blob/f9bad8941ea813982d6dfb10c0377ebf7716b3e7/crates/gpui/Cargo.toml)、
  [GPUI 导出](https://github.com/egoist/zed/blob/f9bad8941ea813982d6dfb10c0377ebf7716b3e7/crates/gpui/src/gpui.rs)。
- 实际发布源码与下载 URL/SHA256 收据：
  `C:/Users/Public/Temp/opencode/gpui-kit-source-verify-efb06a8940104f1cbdedab100c09df0d/source-report.json`。
  三个包各自 `Cargo.toml.orig` 的 `package.metadata.gpui-pre` 指向
  `zed@279fe070bb389b79652e52065b2f001edcc0b11b`。
- 两个 GLM-5.3-Flash 子代理分别提供依赖研究与组件范围研究；本文采用复核后的
  事实，并明确保留未编译的判断，不将子代理的成本推测当作测试结果。
