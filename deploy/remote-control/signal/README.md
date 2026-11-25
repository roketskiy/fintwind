# Fintwind 直连信令服务

本目录是独立的 WebRTC 信令服务，协议版本 2。它只把手机与桌面匹配到同一个房间，
转发受配对密钥保护的信令密文。只支持完整的 offer/answer 交换，不做 trickle ICE。
服务器看不到配对密钥，不解析密文内容，不提供业务中继、TURN 或 HTTP 转发，也不保留
旧中继通道。远控业务数据经 WebRTC 数据通道在两端之间直连。

服务器无法从密文区分信令与业务数据，因此不靠字段名保证内容，而是用严格的次数、
尺寸和时限把交换限制在一次 offer/answer 之内。

本服务与同目录上层的旧 Alma 中继（`../relay/`）互不复用代码。它与手机网页、STUN
共用同一个域名：手机网页与 `/signal/v2` 由上层 `../Caddyfile` 在同一站点提供，
STUN 走独立的 UDP 18762。本目录不再包含独立站点模板。

## 协议

WebSocket 路径为 `/signal/v2`，生产环境经 Caddy 以 WSS 暴露。连接后首条必须是
严格的 JSON 加入帧。

```json
{"type":"join","version":2,"routeHash":"<64位小写hex>","role":"desktop"}
```

`routeHash` 是配对路由编码文本的 SHA-256 小写十六进制。`role` 为 `desktop` 或
`phone`。服务注册角色后回 `{"type":"joined","version":2}`。双方在线时各收到
`{"type":"peer","version":2,"role":"<对端角色>","online":true}`；一方离线时对端收到
`online:false`。同一房间同一角色只允许一个连接，新连接替换旧连接，以支持切换网络后重连。

加入后只接受信令帧。每个 attempt 仅允许 phone 一条、desktop 一条，且 `seq` 必须为 0。

```json
{"type":"signal","version":2,"attempt":"<base64url 32字节>","seq":0,"data":"<base64url密文>"}
```

服务把信令按同房间另一角色转发，并附上 `sender` 字段标明来源角色。`attempt` 标识一次
连接尝试（32 字节），`data` 是不透明密文。服务只校验外层形状与大小，不解析密文内容；
重放与代次绑定由持有配对密钥的两端在密文内处理。任何其它协议类型、多余字段或非零
`seq` 都被拒绝。同一 attempt 内同一角色重复发送即拒绝。手机信令连接关闭或被替换后，
该连接对应的 attempt 结束；迟到的桌面 answer 被丢弃，不会断开桌面的信令连接。

## 边界

- 单条 WebSocket 帧至多 64 KiB，完整 offer/answer（含全部 ICE 候选）须在此之内。
- 每个房间允许突发 16 次连接尝试，之后平均每 15 秒恢复一次额度。触及限额时，
  信令错误帧带 `retryAfterMs`，表示下一次可尝试前的等待时间。每次尝试仅允许 phone
  与 desktop 各一条 seq=0 信令，不提供连续业务转发通道。
- 桌面在线时房间保持开放，WebSocket 心跳用于发现断线。桌面离线后，空闲房间会关闭。
- 接收方积压超过 1 MiB 即断开，不做无界缓冲。
- 未加入的连接受帧数与字节预算限制，10 秒内未加入即关闭。
- 升级请求按 IP 限流，待处理连接数有界，房间总数有界，超限拒绝而非重置计数。
- 服务器不记录路由、密文或 IP 之外的敏感信息；健康检查只返回房间计数。

## 配置

全部通过环境变量配置，写入运营者控制的 environment 文件，不提交仓库。

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `HOST` | `127.0.0.1` | 监听地址，生产置于 Caddy 之后 |
| `PORT` | `8790` | 监听端口 |
| `MAX_ROOMS` | `64` | 同时存在的房间上限 |
| `MAX_CONNECTIONS` | `256` | 进程 TCP 连接上限 |

本服务不需要中继安装准入密钥，也不签发 TURN 凭证。`routeHash` 是本次配对的能力凭证，
信令密文由配对密钥保护。DoS 边界由上面的限流与配额实现。

## STUN

地址发现由同目录上层的仅 STUN 服务（`../stun/`）提供，客户端使用
`stun:<服务域名>:18762`，UDP 18762。本信令服务不签发 TURN 凭证，也不参与地址发现。

## 运行

需要 Node 20 及以上与 `ws` 包。使用锁定依赖安装并禁用安装脚本。

```sh
npm ci --ignore-scripts
node index.mjs
```

`fintwind-signal.service` 是 systemd 单元模板。信令路由已并入上层 `../Caddyfile`：
`@signal path /signal/v2 /healthz` 反代到 `127.0.0.1:8790`，与手机静态页面同域，
因此页面现有的 `connect-src 'self' wss://{$FINTWIND_MOBILE_DOMAIN}` 已覆盖信令 WSS，
无需新增 CSP 来源。旧业务中继路由（`/agent`、`/c/*`、`/v1/admission/*`）已从该模板移除，
落到静态文件服务返回 404，不再转发业务数据。

信令服务监听 `127.0.0.1:8790`，置于 Caddy 之后。安装脚本在上层
`../install-signal-stun.sh`，本目录不含个人 bootstrap 或迁移脚本。

## 安装与发布

发布脚本已迁移到直连信令服务，不再依赖旧中继。

1. 每台主机首次发布前（或信令源码/单元变更后），以 root 运行上层
   `../install-signal-stun.sh`，传入 Node 20+ linux-x64 tarball 的 URL 或本地路径。
   它把完整新版本准备在独立目录 `/opt/fintwind-signal/releases/<id>`：Node 解压到该
   版本的 `node/`，应用源码与锁定依赖（禁用安装脚本）也装进该版本目录。全部就绪后才
   原子切换 `current` 与 `node/current` 符号链接，并 `systemctl restart` 信令服务，
   最后校验 `/healthz`。因此运行中的进程不会被覆盖，健康检查反映的是新代码；旧版本
   目录保留供回滚。缺省 `/etc/fintwind-signal.env` 仅在不存在时写入，保留运营者的值。
   加 `--with-stun` 时同时安装并重启仅 STUN 服务（需先装发行版 coturn 包）。该脚本
   不发布前端、不重载 Caddy、不触碰旧中继。

   脚本在远端主机运行，不假设本机有代理。curl 与 npm 默认遵从进程环境的
   `HTTP_PROXY`/`HTTPS_PROXY`。仅当显式设置 `FINTWIND_INSTALL_PROXY` 时才覆盖代理，
   设为空值则强制直连。下载有 `FINTWIND_INSTALL_DOWNLOAD_TIMEOUT`（秒，默认 180）；
   npm 单次请求超时由 `FINTWIND_INSTALL_NPM_TIMEOUT`（毫秒，默认 120000）控制，整个
   `npm ci` 另有外层总时限 `FINTWIND_INSTALL_NPM_TOTAL_TIMEOUT`（秒，默认 300）。
2. 发布手机静态页面用 `scripts/deploy-mobile-ui.ps1`。它读取上层 `../Caddyfile` 模板、
   替换 `{$FINTWIND_MOBILE_DOMAIN}`、构建并上传，再调用 `publish-mobile-ui.sh`。
3. `publish-mobile-ui.sh` 现在检查 `fintwind-signal.service` 与 `caddy.service` 存活、
   校验信令 `/healthz`，并记录信令 PID。原子切换前端符号链接与 Caddy 站点后，复查信令
   PID 不变且 `/healthz` 仍正常，确认发布没有重启信令。失败时按原有语义回滚前端指针与
   Caddy 站点并重载 Caddy。STUN 是独立 UDP 服务，不在发布门禁内。

## 许可

本服务为 Fintwind 原创实现，随仓库根 `LICENSE` 采用 GPL-3.0。唯一运行时依赖 `ws`
为 MIT 许可。
