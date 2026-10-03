# 通用设置新增「浏览器工具」开关：默认关闭，开启后向模型注入 16 个工具与说明

## 设计决策
- **默认关闭**：开箱/升级后不注入任何浏览器工具与 BROWSER_INSTRUCTION，节省上下文；需要时在通用设置手动开启。设置缺失/非法值一律视为关闭（省上下文是基线，开启是显式动作）。
- **开关语义**：只控「模型侧浏览器工具」（16 个 fintwind_browser_* + 说明）。WebView2 浏览器面板、手动分享、全权模式 UI 不动——不占模型上下文。
- **即时生效，不重启**：插件文件照常注入 OpenCode 常驻进程，daemon 在插件连接建立与设置变更时**推送开关状态**，插件据此 inert 化（context hook 移除工具 + 不注入说明）。避免常驻进程换血，切换立即对下一轮请求生效。
- 双保险：插件侧各工具 execute 在关闭态返回受控错误（提示到设置中开启）。

## 1. 协议层
- `crates/fintwind-protocol/src/browser_tools.rs`：`BrowserToolReply` 新增 `ToolsState { enabled: bool }`（serde 风格与现有一致）。
- `crates/fintwind-protocol/src/settings.rs`：`DaemonSettings::browser_tools_enabled() -> bool` 访问器，读 extra 布尔键 `browser_tools_enabled`，缺省/非布尔 → **false**。

## 2. daemon 注册表与传输 `crates/fintwind-core`
- `browser_tools.rs`：state 增加 `enabled: AtomicBool`（初始即按设置置位）与 `outgoing: HashMap<u64, Sender<BrowserToolReply>>`；`set_enabled(bool)` 存值并向所有连接推送 ToolsState（发送失败惰性清理）；transport 注册/断开时维护 outgoing；单元测试覆盖「注册 sender → set_enabled(false/true) → 收到推送」。
- `browser_tools_transport.rs`：connect 成功后注册 outgoing channel，**先发一条当前 ToolsState**，select 循环同时读 socket 与 channel 并转发到 socket。
- `daemon.rs`：启动初始化与 `Command::UpdateSettings`（:707 现只落盘）两处调用 `browser_tools.set_enabled(settings.browser_tools_enabled())`。serve/attach/browser_tools 挂载链路不动。

## 3. 设置持久化与 UI
- `crates/fintwind-client/src/persistence.rs`：`PersistedState` 加 `browser_tools_enabled: bool`（serde default false、`empty()` 给 false），补进 `app_settings()` 投影与 `apply_app_settings`；`daemon_settings()`（:460）在 extra 里覆盖写入该键——app 字段是真源，覆盖 daemon 文件回读值。
- `src/app/settings.rs`：`render_general_settings`（:570，现为空壳）加一行，抄 Appearance 行布局（:846-877）+ `ui::toggle_switch`（mcp_page.rs:1101 用法）；`set_browser_tools_enabled`：判等短路 → 改 state → `self.save()`（Fintwind::save 已会经 DaemonSupervisor 推送 daemon 设置）→ `cx.notify()`。
- `locales/app.yml`、`zh-CN.yml`、`ja.yml`：`settings.browser_tools` 标题与描述三语，文案说明「默认关闭；开启后向模型提供内置浏览器操作工具，会占用少量上下文」。

## 4. 插件 `resources/opencode-browser-plugin.ts`
- onmessage 处理新 `toolsState` 消息；本地 `bridgeToolsEnabled` **默认 false**（未收到 daemon 状态前不注入）。
- context hook：disabled 时从 `event.tools` 移除全部 fintwind_browser_*（OpenCode 内置浏览器工具 id 照旧无条件移除，防止改用不受控的内置浏览器栈）且不 push BROWSER_INSTRUCTION；enabled 行为与现状完全一致。
- 每个 execute 入口：disabled 时返回受控错误。

## 5. e2e 与收尾
- `scripts/browser-tools-opencode.ts`：**setup 阶段先发 `UpdateSettings{browser_tools_enabled:true}`**（默认关闭下现有 25 项场景才能看到工具），再跑全部既有场景；末尾新增开关场景（EXPECTED_CHECKS 25→26）：关闭 → 断言下一轮请求 toolNames 无任何 fintwind_browser_* 且 BROWSER_INSTRUCTION 标记为 false → 重新开启 → 工具与说明回归。这是「上下文确实省了」的真实行为断言。恢复设置为关闭，不污染后续运行。
- `CHANGELOG.md` unreleased 新增一条（独立新特性）。

## 验证
- `cargo check --workspace` + `-p fintwind --features browser-poc`；`cargo test -p fintwind-protocol -p fintwind-core`（含已知无关失败 resume_location_rules）。
- bun e2e 全量 26/26。
- 用户侧手动：默认无浏览器工具 → 设置开启 → 新消息工具列表与系统提示立即出现；关闭后立即消失（无需重启会话/应用）。

## 明确不做
- 不隐藏/禁用浏览器面板、手动分享、全权模式 UI（不占上下文）。
- 不做 daemon 重启、OpenCode 常驻进程驱逐。
- 不动 serve()/pool/driver 挂载链路（插件永远加载，靠状态位 inert 化）。