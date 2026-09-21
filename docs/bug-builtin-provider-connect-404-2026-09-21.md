# Bug 调查报告：添加内置供应商一律报 404 IntegrationNotFoundError

日期：2026-09-21　|　现场版本：dev（未提交工作树，内置供应商页面开发中）　|　状态：根因已复现定位，未修复

> 更正说明：本报告初版把根因归结为「池化服务器是旧版、缺少新供应商 id」。该结论**错误**，
> 已推翻：日志证明旧版（2.0.9）常驻服务器同样拥有这些 integration，而全新拉起的 2.0.11
> 服务器反而 404。真实根因是下文的「服务器启动竞态」，与版本无关。

## 现场还原

| 时间（本地） | 事件 |
| --- | --- |
| 09-20 22:29 | 一个 opencode 常驻服务（`serve --service`，v2.0.9）启动，此后长时间存活 |
| 09-20 22:32 | 该常驻服务器成功创建 `zhipuai-coding-plan` 凭据（日志 `credential created`）——**旧服务器有这个 integration** |
| 09-20 23:30–23:44 | dev watcher 反复重建重启，Fintwind daemon 连续拉起 **10 个全新的 `opencode serve`**（端口 63718→56804，每 0.5–2 分钟一个） |
| 09-20 23:44（截图1） | 添加智谱 Coding Plan → 404 `IntegrationNotFoundError` |
| 09-21 00:19 | opencode 更新到 2.0.11（对本案无影响，见下） |
| 09-21 00:24–00:28（截图2） | 又一轮重启+新服务器；添加 StepFun Step Plan → 同样的 404 |
| 09-21 00:47:35 | 日志中唯一一次带完整痕迹的 Fintwind connect 请求：**服务器启动后 0.6 秒**收到 `POST /api/integration/stepfun-ai-step-plan/connect/key` → 404 |
| 09-21 00:58 | 一台启动了 4 秒的服务器上同样的请求成功（`credential created`） |

## 结论（TL;DR）

**opencode 服务器的 integration 注册表在进程启动后异步加载，就绪前对任何 connect 请求回答
404 `IntegrationNotFoundError`。Fintwind 的 connect 流程在"按需拉起服务器"后立即发请求，
恰好落在这个窗口里。** 开发期内 watcher 频繁重建重启，每次页面操作都在催生 newborn 服务器，
于是"每次添加都报错"；偶发重试若落在加载完成之后就会成功——这正是日志里 00:47 失败、00:58
成功只差 11 分钟、且版本同为 2.0.11 的原因。

## 复现与测量（本机实测）

完全模拟 Fintwind 的启动方式（`opencode serve --hostname 127.0.0.1 --port N`，Basic 认证），
从服务器可访问的第一刻起高频探测：

- 5 轮独立实验，**首次 connect（age≈0s）5/5 全部 404**，~0.25s 后同请求 204；
- 窗口长度随机：最短 <0.4s，压力下（cargo 重编译同时在跑）观察到 0.6s 仍未就绪
  （即 00:47:35 事故现场）；
- 同一时刻 `GET /api/integration` 可能已返回 200/全量名单，也可能尚未就绪——两个端点
  的就绪时机互相独立，不能互为探针；
- 版本无关：v2.0.9 常驻服务器 22:32 成功连接 zhipuai，全新 v2.0.11 服务器首秒 404。

磁盘上全部 opencode 拷贝（bun 全局、npm cache）均为 2.0.11；`~/.cache/opencode/models.json`
缓存（含全部供应商 id）9 月 19 日起就在；用户级 opencode 配置为空。**排除了所有静态成因。**

## Fintwind 侧的缺陷链

```
用户点击「保存」
  └─ daemon: opencode_pool::acquire(binary, cwd)          // 无活服务器 → 现场拉起
       └─ OpenCodeServer::start (opencode_session.rs:380)
            spawn `opencode serve` → 健康探测轮询(40ms)
       └─ server_is_ready (opencode_session.rs:461)
            探测 /api/info|/api/status|/api/health 任一 2xx 即放行
            ⚠ HTTP 面就绪 ≠ integration 注册表就绪
       └─ native::authorize_integration → POST .../connect/key   // 立即发出
            → 服务器注册表未就绪 → 404 IntegrationNotFoundError
       └─ toast「无法更新 OpenCode 的凭据: …」
```

关键点：每次应用重启后池为空，**第一次 provider 操作必然触发冷启动**，冷启动后请求紧跟着
发出。正常使用中窗口只有亚秒级，多数时候能侥幸通过；但在 watcher 反复重建的开发场景下，
「重启→打开页面→填 key→保存」几乎总是一个 newborn 服务器的第一次业务请求，于是稳定复现。

## 证据链（日志为 opencode 自身日志 `~/.local/share/opencode/log/opencode.log`）

1. `16:47:35.038Z` serve 启动（v2.0.11）→ `16:47:35.642Z` connect 404——**0.6 秒**；
2. `14:32:08Z`（22:32 本地）v2.0.9 常驻服务器 zhipuai **连接成功**——旧版本有此 id；
3. `16:58:46Z` 4 秒龄的 v2.0.11 服务器上 openai + stepfun 双双成功；
4. 23:30–23:44 与 00:46–00:47 两波服务器风暴与开发重建节奏吻合；
5. 本机复现实验：5/5 首次 404、就绪后 204。

## 修复建议

1. **主修复：对 `IntegrationNotFoundError` 做有界重试。** 在 daemon 的
   `AuthorizeProvider`（或 `native::authorize_integration`）里，404 且 `_tag ==
   IntegrationNotFoundError` 时退避重试（如 5 次 × 400ms）。不依赖对 opencode 内部
   加载机制的猜测，任何原因导致的瞬时未就绪都被覆盖。
2. **可选加固：池就绪探测增加注册表维度。** 冷启动后先轮询 `GET /api/integration`
   至 200 再放行第一个业务请求；注意不能以「名单非空」为条件（部分环境可能合法为空），
   也不能用 GET 的就绪代表 connect 的就绪（实测二者不同步）。
3. 顺带发现（独立问题）：Fintwind 对每台服务器以 ~2.4Hz 永久轮询不存在的
   `GET /api/form/request`（30 小时 15.3 万条 404）。应在 native 驱动的事件/表单轮询
   层去除该端点或失败后退避——这是纯粹的性能浪费。
4. 次要建议（初版报告中的名单治理项仍然成立，但优先级降低）：
   - `provider_supports_key`（providers_page.rs:147）用 `providers_key_methods.is_empty()`
     同时表示「未加载」与「加载了空集」，应改为显式加载标记；
   - key 名单加载后，对服务器不认识的 id 禁用提交并说明，而不是放行必败请求；
   - `fetch_integrations` 失败静默降级为 `api.is_some()` 近似，应给出可见状态。

## 遗留疑点

- 截图时刻（23:44 / 00:28）附近的服务器日志里没有对应的 connect 404 记录（toast 会驻留，
  截图晚于请求是常态；00:47 那次有完整痕迹）。不排除个别请求打到了日志不可见的服务器，
  但这不影响根因判定：可复现的失败机制只有一个，即启动竞态。
- 初版报告依据的「00:19 更新 opencode」属实但与本案无关——更新前后行为一致。

## 附：调查过程中的其他发现

- 本机装有多个 opencode 生态组件：官方 desktop（`ai.opencode.desktop`）、MiniMax Hub
  自带的独立 data-home 运行时；排查时需注意区分日志归属。
- `~/.local/share/opencode/log/opencode.log` 只记录非 2xx 响应（外加 SSE 200），
  不能以「日志里没有」推断「请求没发生」——但 404 一定会留痕。
