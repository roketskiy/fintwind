# 浏览器交互与紧凑工具栏修正

## 需求与边界

用户反馈：“UI太丑，人一动页面就失去共享，点击也无效”。本轮取代旧的
“获得原生焦点即接管”策略：普通点击、滚轮、键盘聚焦不撤销共享；明确的
“停止共享”操作才停止自动化与打开能力。切会话、权限降级、关闭和断连
仍须撤销。共享状态与停止入口收进地址栏工具栏，仅监督模式等待审批时
显示操作摘要与审批按钮。最终完整正常应用验收仍由用户进行。
完全访问跨导航保留标签授权；监督模式原有的导航撤销规则未扩大。

## 写实现前列出的失败模式

- 页面鼠标按下、滚轮、原生焦点变化仍隐式撤销共享或取消打开。
- 停止入口只取消当前请求，AI 可立即打开替代标签；打开等待中的停止无效。
- 收起常驻状态栏后，监督模式审批摘要、键盘焦点、拒绝 / 停止入口消失。
- 将手动共享按钮移到工具栏后，事件作用于错误的活动标签或跨会话共享。
- 窄面板中平滑滚动尚未完成即计算坐标；链接换行后大矩形中心落在空白处。
- sticky 区域覆盖目标仍强行点击；不能用 DOM click 绕过真实命中检查。
- 仅从 CSS 推断修好，不通过真实 WebView2 隔离行为验证；不能把隔离验证
  或构建成功称为完整应用实际验收。
- 滚动等待到达上限仍继续发出输入，或监督审批结束后焦点落到禁用控件。

## 审查收尾范围

已完成一轮增量审查。用户明确：“审查花费太多时间了，这一轮不必再大审”，
因此不再启动第二轮审查，仅修滚动超时明确拒绝与审批后焦点回落两处。
遮挡定位过程中滚动属于已有的 scroll-to-target 行为，不是控件点击成功；
拒绝时不派发鼠标输入、不触发控件，不承诺恢复定位前的视口。保留这一能力，
避免把所有视口内被 sticky header 暂时挡住的目标都直接判为不可点击。

## 验证记录

以下区分本轮各次源码状态与报告；此前 30/30 报告不作为本轮通过证据。
本轮 WIP 基线保存在 `C:/Users/Public/Temp/opencode/browser-interaction-baseline/`，
用来区分此前未提交实现与本轮修改。不开 watcher，不做视觉测试，不提交或推送。

- `bun run browser:tools-opencode`：20/20，通过。真实 OpenCode 2.0.16，provider
  与 GUI 是隔离模拟，不代表真实 GLM 模型或完整产品 GUI 已验收。报告：
  `target/browser-tools-opencode/runs/1e775321-c84c-420d-8278-0caa2878df94/report.json`。
- `bun run browser:tools`：两次均 28/30，失败保留，不标为通过。两项分别为
  `unregistering-the-launcher-fails-a-pending-open-but-keeps-pages` 和
  `a-duplicate-launcher-is-refused-and-another-session-launcher-is-invisible`。
  对应 core 源码本轮尚未修改，原因另行只读排查；不能仅据此称为偶发或忽略。
  报告：`target/browser-tools-e2e/runs/a0827996-f505-41bd-be00-96f01a14dc8c/report.json`
  与 `target/browser-tools-e2e/runs/9d1be8bd-e735-4c28-8064-eaecd0fd64c5/report.json`。
- 定位到 example 的跨连接注册竞态：`publish_browser_host` 仅入队，场景 24
  立即从另一条连接打开，owner 日志没有收到预期请求；场景 24 提前失败后
  未清理 launcher，使场景 25 的“本会话无 launcher”前提失效。
  修正仅限 `crates/fintwind-core/examples/browser_tools_e2e.rs`：注册后在 owner
  同一 FIFO 连接上等待 `BrowserList` 往返，并在场景 24 返回前清理、确认
  launcher 撤回。不增加固定 sleep，不重试产品动作，不改 core 产品授权逻辑。
- 修正 fixture 后 `bun run browser:tools` 两次连续 30/30，通过。报告：
  `target/browser-tools-e2e/runs/ba4639fe-a759-4aab-a3b9-78f4f86d8298/report.json`、
  `target/browser-tools-e2e/runs/3eb51f1a-2b16-4fa4-9b8c-62e481c05149/report.json`。
  第二次没有重建，验证同一二进制重复通过；之前失败报告继续保留。
- `cargo check --locked --workspace --all-targets --features browser-poc`：
  当前动作脚本 format 转义修正后通过；格式化后再次检查也通过。正常产品构建也通过：
  `cargo build --locked --package fintwind --bin fintwind --package fintwind-daemon --bin fintwind-daemon`。
  仅保留既有 ModelsDevCost 未使用与 proc-macro-error2 future-incompat 警告。
  格式化后再次正常构建通过；下表为审查收尾前正常构建哈希。
  此条仅记录当时状态，收尾后的最终验证与构建见文末。
- 首次真实原生 runner：36 项中 35 项通过，报告
  `target/browser-bridge-e2e/runs/a2d34849-dccc-4910-a7c7-7f4cc189dd9d/report.json`。
  唯一失败是遮挡拒绝文案匹配：原生返回 `another element covers the target`，
  runner 只匹配 `occlu`。不修改原生行为或删除遮挡检查，修正语义匹配后重跑。
  新增换行链接、超高目标、sticky 底部链接，以及人类 focus / wheel / click
  保持共享、显式停止后旧权限拒绝，均在该次真实适配器运行中通过。
- 格式化后 `cargo check --locked --workspace --all-targets`、
  `cargo fmt --all -- --check` 与 `git diff --check` 通过。
  新增独立诊断脚本曾使 TypeScript 再检查报 unknown 类型错误；结构窄化
  修正后移至 `target/fixtures/browser-locate-script-check.ts`，不作为维护脚本。
  主代理再次执行 `bunx tsc --noEmit --lib ESNext,DOM` 通过。
- 收尾前真实原生适配器 `bun run browser:collaboration --skip-build`：36/36，通过，
  `errors` 为空、cleanup 全通过。最新报告：
  `target/browser-bridge-e2e/runs/8656bbbf-a9fe-4b57-9888-49ece9c0a136/report.json`。
  本轮重建的 PoC host SHA256 为
  `984f69d88cf7c4abcb1d2684a66563bd423b9a627b9996b5469c7eabbdb4b124`。
  仅验证隔离适配器；正常 GUI 工具栏的最终外观、打开流程与用户真实模型
  交互仍由用户自行验收，未执行视觉测试或完整产品自动化。
- 修正旧报告 details 的“状态栏挤占视口”描述后，对同一 PoC host 再运行当前
  runner，仍 36/36，通过。最新当前脚本报告：
  `target/browser-bridge-e2e/runs/19ca6bbf-a74b-4959-bd10-14600a3ecd4d/report.json`。

| 收尾前正常构建（已被文末最终构建取代） | SHA256 |
| --- | --- |
| `target/debug/fintwind.exe` | `96fc3217772d9cff19b21bdd4b2e2fd53da718c5dbf888385e5e34c3c53620df` |
| `target/debug/fintwind-daemon.exe` | `59700e5d4850f64c893f342cb44a8f2b0e5d4fa10ef3b63425563d99a7ec6ae5` |

## 实现依据与交互结构

- 状态所有者仍是 `BrowserView`；GUI 订阅 toolbar 发出的 `ShareRequested`，
  使用订阅绑定的精确 page id 校验 / 创建共享，不根据全局当前页面猜测。
- 地址栏右侧的紧凑控件显示共享 / 已共享 / 正在打开；共享和打开状态带停止
  图标。键盘 Tab 可聚焦，Enter / Space 执行。监督审批摘要保留滚动与 Escape。
- 停止操作在 entity 内撤销 guard、取消 pending / running，打开等待保留
  显式 stop 标记；GUI 据此撤回 launcher。普通 focus 回调仅通知焦点重绘。
- 采用既有主题与 `ActivationExt`，新增控件间距使用 rem 尺度，文字通过既有
  `ui_px` 遵循用户字号设置，无常驻动画。
- 核对了 Cargo.lock 固定的 Zed fork revision
  `f9bad8941ea813982d6dfb10c0377ebf7716b3e7` 的
  `crates/ui/src/components/button/button.rs` 与 GPUI styled API；按紧凑原生
  toolbar、状态与焦点分离的结构实现。Context7 仍因 OAuth 失效不可用。

## 增量审查与最终两处收尾

- 一轮 `code-reviewer` / Step 5 Preview 增量审查完成，不重开旧第三阶段审查。
  审查确认精确页面事件、打开中显式停止、暂停 launcher 防止替代标签及
  共享焦点策略；报告包含 P3 焦点回落、未消费 settle 超时和语言死键。
- 两处局部修正：共享控件启用判断与焦点恢复共用一项内存策略；不可用时
  回落浏览器根焦点。`settle()` 未在 600ms 内稳定立即返回结构化错误，
  不继续派发点击或输入；不自动重试。
- 增补现有原生 E2E 一项“页面持续滚动时等待超时拒绝，不派发点击”，总计
  37 项。先写此失败场景，再改超时处理，没有新增单元测试或完整产品入口。
- 遮挡检查命名从“page is untouched”改为“control is untouched”，对应已有
  断言“不触发目标、不旁路真实命中检测”。定位滚动的副作用边界见上文。
- 用户要求不再大审，本次不启动第二轮；不为语言死键扩大无功能收益清理。
  上表哈希与上述 36 项报告对应收尾前状态，最终重建 / 37 项报告见下文。
- 收尾后首跑 37 项行为全通过、cleanup 全成功，但 runner 总数仍写 36，故
  总报告如实标记失败：
  `target/browser-bridge-e2e/runs/7704ea08-07b8-4406-a3ba-0bb689abd8be/report.json`。
  更新预期为 37 后重跑，不删除此失败报告，不更改任何产品拒绝行为。
- 收尾源码正常 / `browser-poc` 两种 workspace/all-targets 检查均通过；临时
  检查脚本的 `core.autocrlf=false` 将 CRLF 误报为行尾空白，导致尚未执行构建。
  增加仅对此命令生效的 `core.whitespace=trailing-space,space-before-tab,cr-at-eol`
  后差异检查通过；仍检查行尾空格和 tab 前空格，不改全局 Git 配置或其他文件。
- 更新计数后的 `bun run browser:collaboration --skip-build`：37/37，通过，
  总状态为 `passed`、`errors` 为空、cleanup 全成功。报告：
  `target/browser-bridge-e2e/runs/7ead4693-29a7-4ba8-ac92-1b93a3f260cb/report.json`。
  此次仅 runner 预期计数改变，复用上一次已从收尾源码重建的原生宿主；
  host SHA256：`b1809da5ea886526182f538919490d41c4a96c1401c0b362beef41fba84c2d08`。
- 收尾后 `bunx tsc --noEmit --lib ESNext,DOM` 通过。正常应用尚未启动，
  正常应用工具栏外观、完整打开流程和真实模型验收仍交由用户完成。

## 最终交付构建

- `cargo fmt --all -- --check` 通过。
- `cargo check --locked --workspace --all-targets` 通过。
- `cargo check --locked --workspace --all-targets --features browser-poc` 通过。
- 按上述 CRLF 设置执行的 `git diff --check` 通过，最后文档更新前再次通过。
- `cargo build --locked --package fintwind --bin fintwind --package fintwind-daemon --bin fintwind-daemon`
  通过；没有启用 `browser-poc`，没有启动应用、停掉安装版或替换安装文件。
- 仅保留既有 `ModelsDevCost` 未使用与 `proc-macro-error2` future-incompat 警告。
  下表哈希取自上述正常构建，之后仅更新本文档；原生隔离报告为上述 37/37。

| 最终正常构建（两个文件须放在同一目录） | SHA256 |
| --- | --- |
| `target/debug/fintwind.exe` | `e3f1c6250f8ca37144c082d56b6d7869241e0013498592c7b9a895288dfe7a34` |
| `target/debug/fintwind-daemon.exe` | `59700e5d4850f64c893f342cb44a8f2b0e5d4fa10ef3b63425563d99a7ec6ae5` |

完整正常应用 GUI、工具栏外观和真实模型验收尚未执行，由用户自行完成，
步骤见 [browser-manual-acceptance.md](browser-manual-acceptance.md)。本轮不再追加审查。
