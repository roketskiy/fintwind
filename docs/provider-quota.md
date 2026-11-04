# 供应商额度说明

内置供应商详情显示供应商上报的订阅窗口或 API 余额，不含自定义端点。凭据取自
daemon 主机上 OpenCode V2 的认证 `/api/credential` 接口，不读旧认证文件、浏览器
会话或桌面渲染器；每次只查询当前选中的已存凭据。OpenCode 仍是 token 刷新的
所有者，过期的 OAuth 凭据不会由 Fintwind 刷新或改写。

已存凭据选择使用 OpenCode 有序集成连接列表的第一条（用户要求 OpenAI 路径保持
不变除外）；所选 ID 缺失时直接失败，不猜测。同一账户的 401/403 清掉旧读数；
余额缺失不等于零，零余额是合法读数；不同币种与主副额度、按模型/调用/周额度
分开显示。

初始支持：Codex 兼容 OpenAI OAuth、公共 GitHub Copilot OAuth、带项目的 Google
Code Assist OAuth、智谱/Z.ai Coding Plan、Kimi 代码、MiniMax Token Plan、DeepSeek
API 余额、StepFun 国内 API 余额。Claude CLI 与 StepFun Step Plan 浏览器会话不在
范围内；Step Plan 供应商不显示无关的预付 API 余额。

部分供应商的映射参考了开源项目 Magpie 与 CodexBar 的实现，第三方许可见
`provider-quota-upstream-license.txt`。
