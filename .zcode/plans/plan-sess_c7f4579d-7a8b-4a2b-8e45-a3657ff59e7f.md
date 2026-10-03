# fintwind 浏览器协作扩展：Playwright 式操作面 + 截图 + 坐标 + evaluate + 多标签

## 已确认的决策
- **录制暂缓**，本轮不做（选型问题留待单独一轮）。
- **evaluate 与其他变更动作同等授权**：手动分享模式逐次审批（审批条预览表达式前缀），全权自动模式直通。
- **截图走 data URI 内嵌**：新增有界 `Media` 结果通道，本地/远程 daemon 均可用。
- **新动作全清单**：press、select、hover、double_click、drag、click_at、screenshot、evaluate、close。
- 保持原设计思想：失败关闭、scope 四元组处处校验、数据有界、输出标记不可信、人为接管即撤销。

## 1. 协议层 `crates/fintwind-protocol/src/browser.rs`
- 新常量：`MAX_BROWSER_EXPRESSION_BYTES = 32KiB`、`MAX_BROWSER_MEDIA_BYTES = 3MiB`（原图）、`MAX_BROWSER_MEDIA_RESULT_BYTES = 5MiB`（序列化 Media）、`MAX_BROWSER_COORDINATE = 8192`、`MAX_BROWSER_KEY_BYTES = 32`。
- `BrowserAction` 新变体（serde 形态与现有一致）：`Screenshot { full_page }`、`Evaluate { expression }`、`ClickAt { x, y }`、`DoubleClick { selector }`、`Press { selector, key }`、`Hover { selector }`、`Select { selector, value }`、`Drag { from, to }`、`Close`。
- `validate()`：表达式非空 ≤32KiB；坐标 0..=8192；key 走白名单校验函数 `validate_key`（功能键/方向键/单个可打印 ASCII/Control|Shift|Alt 组合）；selector 类字段复用既有规则；Select value ≤ 8KiB。
- `requires_approval()` 不变（仅 Snapshot 免审批，新动作默认需审批）。
- `BrowserResult` 新增 `Media { mime, data }`（base64）。
- 修订模块文档注释：「绝不传输原始脚本」改为受控策略——Evaluate 是显式授权的页面 JS 能力（隔离世界、有界、审批门控、输出不可信），仍拒绝任意 CDP 方法名与 cookie 专递。

## 2. 大小上限贯通（三处强制点变 Media 感知）
- `crates/fintwind-client/src/client.rs:263` `complete_browser_request`：Ok/Error 保持 32KiB，Media ≤ 5MiB。
- `crates/fintwind-core/src/browser_broker.rs:939` `enforce_result_bound`：同规则。
- `crates/fintwind-core/src/browser_tools_transport.rs:42,241`：读路径（插件→daemon）保持 32KiB；daemon→插件回复按结果类型放宽（实现时验证 tungstenite 写路径不受 max_frame_size 限制，若受则单独调帧上限并在读侧用结构校验兜底）。

## 3. GUI 执行层 `src/browser/collaboration.rs`
- 审批摘要 match（813-830）补全，新增 tr! 键（见 §7）。
- `execute()`（1171-1241）新增实现：
  - **Screenshot**：新辅助 `cdp_capture`（超时 10s、字节上限 6MiB）调 `Page.captureScreenshot {format:"png", captureBeyondViewport}`；解码后 >3MiB 降级 JPEG q=80 重截，仍超报错（无新依赖）；返回 `Media`。
  - **Evaluate**：隔离世界 `Runtime.evaluate`（awaitPromise、returnByValue、3s 超时、128KiB 结果守卫、exceptionDetails→受控错误）。
  - **ClickAt**：先取 `{innerWidth,innerHeight}` 校验坐标在视口内（否则明确报错），再 `Input.dispatchMouseEvent` press/release（Issued 完成语义）。
  - **DoubleClick**：复用 click 目标解析 JS，一次 press/release clickCount:2。
  - **Press**：目标解析 + focus（照 fill 的 focus 步骤），key 查表映射 (key, code, windowsVirtualKeyCode, modifiers)，input_pair keyDown/keyUp。
  - **Hover**：目标解析后单发 mouseMoved。
  - **Select**：隔离世界 evaluate：解析目标（ref/CSS 同一约定）、须为非禁用 `<select>`、按 value 匹配 option（找不到报错）、赋值并派发 input+change。
  - **Drag**：解析 from/to 两点，press(from)→插值 mouseMoved→release(to)，全程 guard 检查，Issued 完成。
  - **Close**：execute 层拒绝（app 层拦截，同 Open 的分工）。

## 4. 应用层 `src/app/browser_collaboration.rs`
- `dispatch_browser_request`（640-715）在 Open 特例后加 **Close** 特例：校验 scope/页/授权匹配 + `browser_full_access()` + 页面属于 `automation_pages`；手动模式完成错误「仅自动打开的页可由会话关闭」；通过则**先** `complete_browser_request(Ok)`、从 `automation_pages` 移除（避免触发 forget 的 pause 分支），再按 page_id 复用 `close_right_panel_surface` 拆除（既有实体移除 + 分享集重发布即撤销）。

## 5. 插件 `resources/opencode-browser-plugin.ts`
- 新工具（保持 codemode:false、warning 前缀、ref|selector 单字段约定）：`fintwind_browser_screenshot`、`_evaluate`、`_press`、`_select`、`_hover`、`_double_click`、`_drag`、`_click_at`、`_close`；`FINTWIND_BROWSER_TOOL_NAMES` 扩展为 16 个。
- `request()` 返回完整结果对象；`createTool` 组装 content：普通结果 JSON 字符串；Media 结果 `content: [{type:"text",text:...},{type:"file",uri:"data:<mime>;base64,...",mime,name}]`（已验证 v2.016 支持 FileContent，模型可见图像）。
- `onmessage` 大小分叉：文本回复 36KiB，Media 回复 ~5MiB。
- `BROWSER_INSTRUCTION` 增补：evaluate 审批与 cookie 可读性明示、截图为视觉观察、close 仅自动打开页、坐标点击前先快照确认。

## 6. PoC 与 e2e
- `src/browser_poc.rs:941` kind 映射补全（browser-poc feature）。
- `crates/fintwind-core/examples/browser_tools_e2e.rs:422` wire kind 映射补全；新增行为测试：validate 边界（表达式超限/坐标越界/非法 key）、Media 大小感知 bound、Media 结果经 transport 回显不截断。
- `scripts/browser-tools-opencode.ts` / `browser-tools-native.ts` 增加 evaluate/screenshot/press 场景。

## 7. 本地化（app.yml + zh-CN.yml + ja.yml，三语全上）
新键：`browser_collaboration.{screenshot_summary, evaluate_summary, click_at_summary, double_click_summary, press_summary, hover_summary, select_summary, drag_summary}`。

## 8. 依赖与验证
- 零新增依赖（base64 0.22 已直接依赖；截图降级用 CDP 侧 JPEG）。
- `cargo build --locked` + `cargo test -p fintwind-protocol -p fintwind-core`（新校验为真实行为缺口，符合 AGENTS.md 测试观）。
- 手动验证（用户侧，dev watcher 应用内）：全权会话跑 open→snapshot→evaluate→screenshot→press→close 序列，确认审批文案、截图在对话中可见、close 后分享集撤销；手动分享模式确认 evaluate 审批条出现。

## 明确不做
录制（暂缓）、文件上传、networkidle、console 监听、视口设置、截图落盘目录。