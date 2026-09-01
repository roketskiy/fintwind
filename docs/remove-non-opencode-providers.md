# 移除除 OpenCode 外的所有 CLI 集成

## 1. 背景与目标

Waku 目前集成了 8 个 coding-agent CLI：**Amp、Claude Code、Codex CLI、Cursor CLI、DeepSeek Harness、OpenCode、Grok Build、Pi**。目标是把应用收敛为 **仅支持 OpenCode** 一个 provider，彻底删除其余 7 个的代码路径，包括：

- `ProviderKind` 枚举类型本身（用户决策：彻底删除类型，而非收缩为单变体）
- 各 provider 的驱动、会话辅助、进程池、模型目录、斜杠命令、commit 消息、技能扫描分支
- 桌面端与 Web 端的 provider 选择 UI、provider 设置页
- 用量历史功能（只扫描 Claude/Codex 本地文件，与 OpenCode 无关；**保留** OpenCode Go 的 plan 用量展示）
- 其他 provider 的图标资源、本地化字符串、文档

旧数据库数据不做迁移、不做清理（用户决策："不必理会"），仅保留必要的最小护栏。

## 2. 现状调查

### 2.1 Provider 支持全景

| 层 | 位置 | 现状 |
| --- | --- | --- |
| 核心枚举 | `crates/waku-protocol/src/model.rs:10` | `ProviderKind` 8 变体 + `ALL` 常量 + `id/display_name/short_name/command` + 能力谓词 |
| 恢复游标 | `crates/waku-protocol/src/model.rs:132` | `ProviderResumeCursor` 8 变体，字段各异（thread_id / session_id / session_file / fork_context / resume_at） |
| 设置 | `crates/waku-protocol/src/settings.rs` | `disabled_providers: Vec<ProviderKind>`、`provider_binary_overrides: HashMap<ProviderKind, String>` |
| 驱动 | `crates/waku-core/src/driver/` | `codex.rs`(3080)、`claude.rs`(2305)、`acp.rs`(1762，Cursor+Grok 共用)、`deepseek.rs`(1618)、`pi.rs`(1520)、`amp.rs`(786)、`opencode.rs`(1785)；`mod.rs:199 start_local` 按 provider 路由 |
| 会话辅助 | `crates/waku-core/src/` | `amp_session.rs`(315)、`claude_session.rs`(509)、`cursor_session.rs`(121)、`deepseek_session.rs`(827)、`deepseek_pool.rs`(162)、`grok_session.rs`(387)、`opencode_session.rs`(612)、`opencode_pool.rs`(365) |
| 模型目录 | `crates/waku-core/src/model_catalog.rs`、`crates/waku-protocol/src/model_catalog.rs` | 每 provider 各自的模型发现/固定目录（Claude 固定列表、Codex `model/list`、Pi `get_available_models`、ACP handshake 等） |
| 斜杠命令 | `crates/waku-core/src/composer_complete.rs` | 按 provider 的 `.claude/command`、`.codex/skills` 等补齐与 `discover_slash_commands` |
| Commit 消息 | `crates/waku-core/src/git_commit.rs` | 按 provider 分支构造 commit 风格 |
| 技能 | `crates/waku-core/src/skills.rs` | `SkillSource::Provider(ProviderKind)` 扫描 `.claude/skills`、`.codex/skills`、`.cursor/skills`、`.opencode/skills`、`.pi/skills`、`~/.claude/...` 等 |
| 用量历史 | `crates/waku-protocol/src/usage_history.rs`、`crates/waku-core/src/usage_history.rs` | `UsageProvider` 仅 Claude/Codex；扫描 `~/.claude/projects` 与 `~/.codex/sessions` |
| Plan 用量 | `crates/waku-core/src/usage.rs` | Claude OAuth、Codex rate-limit、OpenCode Go、Grok 四个 fetch 函数 |
| 协议消息 | `crates/waku-protocol/src/protocol.rs`、`provider_session.rs`、`workspace.rs`、`git.rs`、`skills.rs` | `Command::ProbeProvider`、`FetchPlanUsage`、`ProviderSessionForkRequest`（claude/amp/cursor/openCode/grok 5 变体）、`WorkspaceOperation::DiscoverSlashCommands`、`AgentInvocation`、`SkillSource` 均携带 provider |
| 桌面 UI | `src/` | `app/composer.rs` provider 选择 tabs（`visible_picker_tabs` 遍历 `ProviderKind::ALL`）、`app/settings.rs` provider 设置页、`app/runtime.rs`(41 处 provider 分支)、`app/usage_page.rs` 历史页、`app/usage_meter.rs` plan 用量、`app/skills_page.rs`、`app/sessions.rs`、`app/sidebar.rs`、`app/background_work.rs`、`ui/mod.rs` provider 颜色/图标映射、`app/tests.rs`(56 处 fixture) |
| Web 前端 | `apps/web/src`、`packages/waku-client/src/generated` | provider 选择、settings-view、model-picker、usage-chart/usage-settings、waku-icon、provider-probe-cache、composer-preferences、daemon-api |
| 资源 | `assets/icons/provider-*.svg` | amp、claude、cursor、deepseek、grok、openai、opencode、pi 共 8 个 |
| 本地化 | `locales/*.yml` | `providers.*`、`settings.providers*`、`usage_error.*` 等键 |
| 文档 | `docs/providers.md`(740 行) | 8 provider 集成细节；`AGENTS.md`、`README.md` 亦有提及 |

### 2.2 `ProviderKind` 引用分布（行数）

```
83  crates/waku-protocol/src/model.rs        28  crates/waku-core/src/daemon.rs
56  src/app/tests.rs                         28  crates/waku-core/src/model_catalog.rs
41  src/app/runtime.rs                       24  crates/waku-core/src/composer_complete.rs
37  crates/waku-core/src/persistence.rs      22  crates/waku-core/src/git_commit.rs
21  src/app.rs                               21  src/ui/mod.rs
17  crates/waku-core/src/skills.rs           15  src/app/composer.rs
14  src/app/sessions.rs                      14  crates/waku-client/src/persistence.rs
12  crates/waku-core/src/driver/acp.rs       10  src/app/usage_meter.rs
10  crates/waku-protocol/src/model_catalog.rs 9  crates/waku-core/src/driver/mod.rs
 9  apps/web/src/lib/daemon-api.ts           8  apps/web/src/components/settings-view.tsx
 7  src/app/usage_page.rs                    7  apps/web/src/lib/composer-preferences.ts
 7  crates/waku-client/src/composer_complete.rs 7  crates/waku-core/src/server.rs
 6  src/app/settings.rs                      6  apps/web/src/components/waku-icon.tsx
 6  src/app/skills_page.rs                   5  apps/web/src/components/model-picker.tsx
 5  apps/web/src/hooks/use-daemon-data.ts    4  crates/waku-core/src/driver/support.rs
 4  crates/waku-core/src/model.rs            4  apps/web/src/lib/provider-probe-cache.ts
 3  crates/waku-protocol/src/settings.rs     3  apps/web/src/lib/model-picker-presentation.ts
 3  crates/waku-protocol/src/protocol.rs     3  src/app/sidebar.rs
 3  crates/waku-protocol/src/usage_history.rs 2  src/app/render.rs
 2  crates/waku-protocol/src/skills.rs       2  crates/waku-protocol/src/git.rs
 2  src/driver/mod.rs                        2  crates/waku-core/src/settings.rs
 2  crates/waku-core/src/cursor_session.rs   2  crates/waku-core/src/driver/opencode.rs
 2  apps/web/src/components/skills-settings.tsx 2  apps/web/src/lib/composer-autocomplete.ts
 2  crates/waku-protocol/src/workspace.rs    1  src/app/command_palette.rs
 1  src/app/background_work.rs               1  crates/waku-core/src/driver/opencode.rs
 1  packages/waku-client/src/generated/ProviderKind.ts
```

约 60 个源文件直接引用 `ProviderKind`，另有大量文件间接依赖（驱动内部实现、usage 扫描、测试 fixture 等）。

## 3. 目标架构

- 唯一 provider：**OpenCode**（`opencode serve` + HTTP/SSE 传输，`src/driver/opencode.rs` 保持不动）。
- `AgentSession.provider` 字段：由 `ProviderKind` 改为 `String`（新建会话固定 `"opencode"`）。旧库中的 `"claude"` 等值可被宽容反序列化（String 不校验），UI 照常显示会话，但**启动新回合时校验 `provider == "opencode"`，否则拒绝并提示**——这是唯一保留的旧数据护栏，不做任何迁移/清理/隐藏。
- 移除类型（协议层一次性删除）：
  - `ProviderKind`、`ProviderKind::ALL`、`ProviderResumeCursor`（除 `OpenCode` 变体外的 7 个）
  - `UsageProvider`、`DaySlice`、`MonthSlice`、`ProviderSlice`、`ProviderDay`、`ModelSlice`、`ProjectSlice`、`CostQuality`、`PricingStatus`（用量历史整套类型）
  - `ProviderSessionForkRequest`（除 openCode 变体外）
  - `DaemonSettings.disabled_providers`、`provider_binary_overrides`
  - `SkillSource::Provider` 变体（技能来源收敛为共享 + `.opencode/skills`）
- `wire` 协议消息同步瘦身：`Command::ProbeProvider`、`FetchPlanUsage`、`WorkspaceOperation::DiscoverSlashCommands`、`AgentInvocation`、`WireDriverStartOptions`、`ProviderProbe` 移除 provider 参数。
- 驱动路由 `driver::start_local` 不再接收 provider 参数，固定启动 `OpenCodeDriver`。
- plan 用量只保留 `fetch_opencode_go_plan_usage`；`usage_meter` 的 `PLAN_USAGE_PROVIDERS` 收缩为 `[OpenCode]`。

## 4. 实施计划

按依赖顺序分 6 个阶段，每阶段可独立编译、提交。**阶段 0–2 完成后 Rust 侧应 `cargo check` 全绿**；阶段 3 后跑 `bun run db:generate` 确认无 schema 变化、重新生成 TS 绑定；阶段 5 在调试应用中按真实 OpenCode 交互验证。

### 阶段 0：协议层类型收缩（`waku-protocol`）

1. `crates/waku-protocol/src/model.rs`
   - 删除 `ProviderKind` 枚举、`ALL`、全部能力谓词（`supports_*`）。
   - `ProviderResumeCursor` 只留 `OpenCode { session_id }`；`from_session_id`/`provider`/`native_id` 化简为无分支。
   - `AgentSession.provider: ProviderKind` → `String`；`AgentSession::new` 固定写入 `"opencode"`，加 `pub const PROVIDER: &'static str = "opencode"`（或直接内联）。
   - `AgentInvocation`（`crates/waku-protocol/src/git.rs` 附近）删除 `provider` 字段。
   - 清理 `model_catalog.rs` 中 Claude 固定列表与其他 provider 目录函数，只留 OpenCode 的。
   - 测试：fixture 中 `ProviderKind::Codex` 等全部替换（模型层测试较少，先改）。
2. `crates/waku-protocol/src/settings.rs`：删 `disabled_providers`、`provider_binary_overrides` 及默认值；迁移旧设置文件的读取（见 §5）。
3. `crates/waku-protocol/src/protocol.rs`：`Command::ProbeProvider` 删 provider 参数（保留命令本身，便于 UI 复用探测逻辑）；`FetchPlanUsage` 删 provider 参数；`WireDriverStartOptions` 删 provider 字段。
4. `crates/waku-protocol/src/provider_session.rs`：`ProviderSessionForkRequest` 只留 `openCode` 变体（或整体删除，改由通用结构承载）。
5. `crates/waku-protocol/src/workspace.rs`：`WorkspaceOperation::DiscoverSlashCommands` 删 provider 字段。
6. `crates/waku-protocol/src/skills.rs`：`SkillSource` 删 `Provider(ProviderKind)` 变体；若只剩 `Shared`，评估是否直接删除该类型并在 `waku-core/src/skills.rs` 用常量路径替代。
7. `crates/waku-protocol/src/usage_history.rs`：整文件删除（用量历史类型全部移除）。
8. `crates/waku-protocol/src/driver_wire.rs`、`runtime_event.rs` 等：顺带清理引用。

### 阶段 1：核心层（`waku-core`）

1. **删除文件**：
   - `src/driver/{acp,amp,claude,codex,deepseek,pi}.rs`
   - `src/{amp_session,claude_session,cursor_session,deepseek_session,deepseek_pool,grok_session}.rs`
   - `src/usage_history.rs`
   - `src/model.rs` 中的 per-provider 残留（若有）
2. `src/driver/mod.rs`：`start_local` 删 provider 参数与 match 路由，直接 `OpenCodeDriver::start`；`DriverStartOptions` 删 provider 相关字段（保留 `provider_cursor`）。
3. `src/driver/support.rs`：删除对其他 provider 的探测/二进制发现逻辑（保留 OpenCode 的 `opencode` 探测；`command_env` 中 `SHELL` 等共用逻辑保留）。
4. `src/opencode_session.rs`、`src/opencode_pool.rs`：保留，清理其中 provider 参数的引用。
5. `src/skills.rs`：删 `.claude/skills`、`.codex/skills`、`.cursor/skills`、`.pi/skills`、`~/.claude/...` 等扫描；保留 `.opencode/skills` 与 shared。
6. `src/composer_complete.rs`：删 Claude/Codex/Cursor/Amp/Pi/DeepSeek/Grok 分支与 `discover_slash_commands` 的 provider 参数，只留 `.opencode/command` 等 OpenCode 规则。
7. `src/git_commit.rs`：commit 消息生成只留 OpenCode 分支；`AgentInvocation` 调用点删除 provider 传参。
8. `src/usage.rs`：删 `fetch_claude_plan_usage`、`fetch_codex_plan_usage`、Grok fetch（若有）；只留 `fetch_opencode_go_plan_usage` 及其辅助。
9. `src/model_catalog.rs`：删其他 provider 的模型发现（throwaway 进程探测、ACP handshake 目录等），只留 OpenCode 的。
10. `src/daemon.rs`、`src/server.rs`、`src/persistence.rs`、`src/workspace.rs`、`src/projectless.rs`、`src/computer_use.rs`（若含 provider 分支）、`src/checkpoint.rs`：清理 provider 参数与分支；`provider_probe`、`fetch_plan_usage` 处理器不再按 provider 分发。
11. `src/settings.rs`：`DaemonSettings` 读写适配新结构；旧设置文件中的 `disabled_providers`/`provider_binary_overrides` 在读取时忽略（serde 默认丢未知字段，确认无 `deny_unknown_fields`）。
12. `crates/waku-client/src/persistence.rs`、`crates/waku-client/src/composer_complete.rs`：同步清理。
13. 核心层测试：`server.rs`、`settings.rs`、`git_commit.rs`、`composer_complete.rs`、`skills.rs`、`usage.rs` 等测试的 fixture 全部改为 OpenCode 路径；删除针对已删驱动的测试（codex/claude/pi/amp/acp/deepseek 的单元与 `#[ignore]` 集成测试）。

### 阶段 2：桌面 UI（`src`）

1. `src/app/composer.rs`：删除 provider 选择 rail（`visible_picker_tabs`、`provider_picker_menu` 等）；默认/锁定 provider 固定 OpenCode；`state.disabled_providers` 引用删除。
2. `src/app/settings.rs`：删除 `SettingsPage::Providers` 页（`render_providers_settings`、`render_provider_expanded_settings`、provider 探测/启用/二进制覆盖 UI）；保留其余设置页；`refresh_provider_detection` 收缩为一次性探测 opencode（若仍有需要，如首次启动检测）。
3. `src/app/runtime.rs`（41 处分支）：`ensure_driver`、`apply_session_options`、resume/fork/rollback、`retain_runtime_after_cancel`、模式映射（`mode_arguments` 等）全部化简为 OpenCode 路径；`InteractionMode`/`RuntimeMode` → OpenCode 的映射保留（`plan`/`build` agent、permission reply）。
4. `src/app/sessions.rs`：`retain_runtime_after_cancel` 简化为 `true` 常量路径；`sessions.rs:7` 的 provider 判断删除。
5. `src/app/usage_page.rs`：整文件删除；sidebar/composer 中"用量历史"入口删除。
6. `src/app/usage_meter.rs`：`PLAN_USAGE_PROVIDERS` 收缩为 `[ProviderKind::OpenCode]`（或常量）；Grok 特殊 cadence 删除；plan 面板只渲染 OpenCode Go 一条。
7. `src/app/skills_page.rs`、`src/app/sidebar.rs`、`src/app/background_work.rs`、`src/app/command_palette.rs`、`src/app.rs`、`src/app/render.rs`：清理 provider 引用（skills 来源、DeepSeek 特殊处理、provider 显示等）。
8. `src/ui/mod.rs`：`provider_color`/`provider_icon` 固定返回 OpenCode 的颜色与图标（或删除函数、调用点内联）；删除其余 provider 图标引用。
9. `src/driver/mod.rs`（UI 侧代理）：`start_remote` 删 provider 参数。
10. `src/app/tests.rs`（56 处）：fixture 的 `AgentSession::new(project_id, ProviderKind::Codex)` 等全部改为 `AgentSession::new(project_id)`；按新签名调整。

### 阶段 3：Web 前端与 TS 绑定（`apps/web`、`packages/waku-client`）

1. `bun run codegen`（或项目既有的 ts-rs 生成脚本）重新生成 `packages/waku-client/src/generated/*.ts`：`ProviderKind.ts` 删除；`ProviderKind` 类型引用从各生成文件消失。
2. `apps/web/src`：
   - 删除 provider 选择 UI（composer 中的 provider 切换、`composer-preferences.ts` 中的 provider 偏好）。
   - `settings-view.tsx` 删除 providers 设置区（disabled、binary override、探测）。
   - 删除 `usage-chart.tsx`、`usage-settings.tsx` 及用量历史页面/路由（保留 plan 用量展示若有）。
   - `waku-icon.tsx`、`model-picker.tsx`、`model-picker-presentation.ts`、`provider-probe-cache.ts`、`daemon-api.ts`、`use-daemon-data.ts`、`skills-settings.tsx`、`composer-autocomplete.ts`、`transcript.tsx`、`sidebar-presentation.ts` 等清理 provider 引用。
   - 相关 `.test.ts`/`.test.tsx` 同步更新。
3. `apps/web/src/lib/event-reducer.ts`、`runtime-context.tsx`：清理 `session.provider` 分支（若仍读取该字段，保持 String 读取即可）。

### 阶段 4：资源、本地化、文档

1. 删除 `assets/icons/provider-{amp,claude,cursor,deepseek,grok,openai,pi}.svg`，保留 `provider-opencode.svg`。
2. `locales/app.yml`、`ja.yml`、`zh-CN.yml`：删除 `providers.*`、`settings.providers*`、`usage_error.*`（claude/codex 相关）、`usage.*` 历史页相关键；保留 OpenCode 相关；核对 `mode.*`（访问模式文案仍通用）。
3. `docs/providers.md`：重写为单 provider 文档（OpenCode serve 传输、approval、steer、fork、model discovery、computer use）。
4. `AGENTS.md`、`README.md`、`CHANGELOG.md`、`website/`：更新产品描述（"supports OpenCode"）。
5. `db/schema.ts`：`sessions.provider` 列**保留**（text，旧数据继续存在）；`bun run db:generate` 确认无新迁移产生。
6. `.github/workflows`、`scripts/`：检查是否有 provider 相关的 CI/打包引用。

### 阶段 5：测试与验证

1. `cargo check` / `cargo test`（单元）全绿；`bun test`（web）全绿。
2. 删除/替换引用已删模块的测试；确认无 `ProviderKind` 残留：`rg "ProviderKind|UsageProvider|claude_session|codex.rs|PiDriver|AmpDriver|AcpDriver|DeepSeekDriver" crates src packages apps` 应为空（白名单除外）。
3. 调试应用实测（真实 OpenCode 交互）：
   - 新建会话、多轮对话、resume（重开应用后续聊）
   - Supervised 模式的 permission 请求、Plan/Build agent 切换
   - 模型切换（model discovery）、上下文用量表
   - 斜杠命令补齐、技能发现（`.opencode/skills`）
   - fork/rollback、stop、steer（⌘↩）
   - 设置页无 provider 残留；旧 provider 会话可见但无法启动新回合（护栏提示）
4. 旧数据冒烟：保留一份含 `provider: "claude"` 会话的数据库启动应用，确认不崩溃、旧会话只读展示。

## 5. 兼容性与旧数据处理

| 数据 | 处理 |
| --- | --- |
| `sessions.provider`（DB） | 列保留，旧值不动。`AgentSession.provider` 改为 `String`，反序列化宽容接受任意旧值 |
| 旧会话继续对话 | 启动护栏：`provider != "opencode"` 时拒绝启动新回合并提示"该会话由不再支持的 X 创建，仅可查看"。不做迁移、不隐藏、不删除 |
| `session_details` JSON | 含 `ProviderResumeCursor` 旧变体——`ProviderResumeCursor` 枚举收缩后，反序列化旧值将失败。策略：为 `provider_cursor` 字段保留宽容反序列化（`Option<Value>` 兼容层或 `#[serde(default)]` + 自定义反序列化器丢弃无法识别的变体），确保打开旧会话不崩溃 |
| 设置文件 | 旧 `disabled_providers`/`provider_binary_overrides` 键被 serde 忽略（确认 `DaemonSettings` 无 `deny_unknown_fields`）；不清除磁盘上的旧文件 |
| 会话 JSON 序列化 | 新保存统一写 `"provider": "opencode"`；`ProviderResumeCursor` 只序列化 OpenCode 变体 |

## 6. 风险与注意事项

1. **反序列化兼容**（最高风险）：`ProviderResumeCursor` 收缩后旧 `session_details` 无法反序列化。必须在阶段 0 一并加宽容反序列化层，并用旧数据实测（见阶段 5-4）。
2. **测试面大**：`src/app/tests.rs`、`server.rs` 等大量测试以 `ProviderKind::Codex` 为默认 fixture，删除类型后编译错误会集中爆发；建议在阶段 0 就批量替换为无参构造，而不是等编译器逐个报错。
3. **Web 端生成代码**：`generated/*.ts` 由 ts-rs 生成，必须与 Rust 端同步重新生成，否则 TS 编译失败；CI 中若有生成校验需保持一致。
4. **usage 页面删除影响面**：`usage_page.rs` 入口在 sidebar/composer 多处；删除时同步清理路由、locale 键、`daemon-api.ts` 中的 `LoadUsageHistory`/`UsageHistory` 消息与 `waku-client` 类型。
5. **`computer_use`**：目前 Codex/Pi/OpenCode/Grok 各自实现；只保留 OpenCode 的 `OPENCODE_CONFIG_CONTENT` 路径，确认 `computer_use.rs` 与 `src/computer_use.rs` 无其他 provider 残留。
6. **`agent_preset`**：目前仅 DeepSeek Harness 使用；删除 DeepSeek 后可评估是否移除该字段（保留亦可，OpenCode 无需）。
7. **icon 引用**：`ui/mod.rs` 与 web `waku-icon.tsx` 引用图标路径，删除 svg 前先清引用，避免 asset 加载报错。

## 7. 验收标准

- [ ] `rg "ProviderKind" crates src packages apps` 无结果（除历史迁移注释）
- [ ] 仅 OpenCode 一个 provider：新建会话、设置页、composer 均无其他 provider 入口
- [ ] 删除的驱动/会话文件无任何引用，`cargo check`、`cargo test`、`bun test` 全绿
- [ ] 调试应用真实 OpenCode 全流程可用（§阶段 5-3 清单）
- [ ] 含旧 provider 数据的库启动正常，旧会话只读、无崩溃（§阶段 5-4）
- [ ] 用量历史入口消失，plan 用量（OpenCode Go）正常展示
- [ ] docs/providers.md 与产品文案同步更新
