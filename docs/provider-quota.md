# 供应商额度说明

用量页将用量统计与订阅余额整合在同一仪表盘中：顶部并排展示“总 Tokens”“生成 Tokens”“平均缓存命中率”与“订阅余额”四张指标卡片，点击前三张卡片可切换主趋势图的指标，主趋势图支持对比上一周期；底部提供按模型、提供商、项目与全量构成切换的渠道分布卡片，以及可在 6 周周矩阵与 26 周热力图间切换的活跃分布卡片。

订阅余额卡片只显示 OpenCode 已连接且具有额度适配的供应商，供应商详情页不再重复显示。进入用量页和手动刷新时，先读取当前 OpenCode 的连接列表，再查询额度；未连接的服务不出现。已连接但凭据失效、额度读取失败或无法查询时，保留对应状态，不把服务直接隐藏。卡片每页显示最多 3 个服务并支持翻页，点击任意服务可展开其多窗口或余额明细，也可单独刷新或通过右上角总刷新按钮一并更新；订阅余额不受历史日期和项目选择影响。卡片复用 Ely GPUI Components 的控件、图表和状态展示，表面采用 Fintwind 统一的圆角、边界和阴影，并跟随主题和字体设置。

用量统计提供日期筛选（默认近 7 天，另有单日与日期选择、近 30 天、全部），并在存在有效费用数据时于摘要和排行提示中附带展示费用。查看订阅余额或点击卡片内刷新时，不会重新扫描历史会话。

额度来源分三类：本机订阅（Claude Code、Codex）读运行 Fintwind
的那台机器，与所选远端 workspace 无关；其余供应商沿用所选 OpenCode
环境的已存凭据（有序连接第一条，环境变量连接按 models.dev 登记的变量
名精确匹配）。Claude/Codex 也必须在 OpenCode 中具有对应连接才显示，
但额度来源仍是本机 CLI 登录，不会改成远端主机的账户。

OpenCode Console 使用连接 ID `opencode`，直接调用当前 OpenCode 服务的
`/api/experimental/usage/limits`，由服务解析其 Console OAuth、工作空间
及 Go 订阅。无需另建 `opencode-go` API Key 连接，不导出 Console token。
Go 分别显示滚动、每周和每月窗口；接口未提供的预算或余额不推算为零。

本机 Claude 通过 `claude auth status --json` 读取身份，再按 Magpie 的
默认交互模式，在独立的 Windows 伪终端中输入 `/usage`，读取 CLI 显示的
订阅窗口、重置时间及额外用量状态，不直接请求 Anthropic 的额度接口，
也不回退到 `-p`。终端按屏幕单元格还原增量重绘，并保留面板最后一次
显示的读数；未加载完成或读取失败时保留旧值，不伪造窗口。

交互会话使用 Fintwind 专用工作目录。与 Magpie 一样，首次运行时会回答
该目录的信任问题及引导界面，Claude CLI 因而可能更新其信任或引导配置；
登录界面交给用户处理。只输入本地命令 `/usage`，不向模型发送提示词。
交互式额度读取目前仅提供 Windows ConPTY 适配。

本机 Codex 只读 `CODEX_HOME`（默认 `~/.codex`）的
`auth.json`（as-kept，不刷新写回），凭该 token 请求 ChatGPT 后端
`wham/usage`；加密存储或 keyring 独占的登录不可读，记为无凭据。

服务端凭据由 OpenCode 管理，本机订阅凭据由各自 CLI 管理；Fintwind
不刷新或改写凭据文件。同一账户的明确授权失败清掉旧读数，临时失败与
429 保留带时间的旧值；余额缺失
不等于零，零余额是合法读数。连接列表只采用最新请求的结果；切换账户或
OpenCode 环境后，旧请求不能覆盖当前状态。本机登录失效后，旧额度不会因
缓存命中再次显示为有效读数。不同币种与
主副额度、按模型/调用/周额度分开显示；接口未给的字段不造数。

覆盖：Claude Code 与 Codex 本机登录、公共 GitHub Copilot OAuth、带项目的 Google
Code Assist OAuth、智谱/Z.ai（含基础与 Coding Plan）、Kimi 代码、
MiniMax（含基础与 Token Plan）、Command Code、Ollama Cloud、OpenRouter、
硅基流动（双区）、Moonshot（双区）、OpenCode Go、DeepSeek 余额、StepFun
全系余额。无查询接口的内置供应商（Azure、Vertex、Bedrock、xAI、阿里、
OpenCode Zen 等）与需双 key 签名的计划（腾讯 Token Plan、火山 Coding
Plan）不在范围内。这两个计划的上游查询需要独立的签名密钥对，现有
OpenCode 单 key 连接不能直接提供，因而不会发起查询或显示为已接通。

额度映射和 Claude 终端读取参考开源项目 Magpie 的
`43849d51c3273881839a8ef3e5aa7109bd2c8d36` 源码快照，既有映射还包含
CodexBar 的实现。两者均采用 MIT 许可，第三方许可见
`provider-quota-upstream-license.txt`。
