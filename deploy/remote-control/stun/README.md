# Fintwind STUN 地址发现服务

本目录是仅 STUN 的 coturn 配置，供手机与桌面在 WebRTC ICE 中发现各自的公网地址。
它不是 TURN 服务器，不转发任何业务数据或媒体。客户端使用 `stun:<服务域名>:18762`，
UDP 18762。部署地址由运营者决定，本目录只提供配置与说明，不包含部署脚本。

## 仅 STUN，禁 TURN

`turnserver.conf` 用 `stun-only` 让 coturn 忽略所有 TURN 请求（Allocate、Refresh、
ChannelBind、Send、CreatePermission）。同时关闭 TCP 中继端点（`no-tcp-relay`），
并关闭 TCP、TLS、DTLS 监听
（`no-tcp`、`no-tls`、`no-dtls`）。服务器只保留一个明文 UDP STUN Binding 监听。

STUN Binding 无需认证，因此不配置 `lt-cred-mech`、`static-auth-secret`、`user` 或
`realm`。关闭 RFC 5780 NAT 行为发现（`no-rfc5780`），拒绝组播对端
（`no-multicast-peers`）。当前 coturn 默认拒绝环回对端，旧的 `no-loopback-peers`
选项已移除，不再写入。

## 客户端约定

桌面与手机在 ICE 配置中使用 `stun:<服务域名>:18762`，不配置任何 `turn:` 或 `turns:`
地址，也不带用户名或凭证。无直连路径时连接失败并提示切换网络，不回退到中继。

## 安装与运行

需要 coturn 4.6 及以上（提供 `stun-only`）。以发行版包或源码安装 `turnserver`，
本服务不依赖 Node。

1. 创建服务用户 `fintwind-stun`（系统用户，无登录 shell）。
2. 将 `turnserver.conf` 放到 `/etc/fintwind-stun/turnserver.conf`，按主机调整
   `listening-ip`。该文件不含密钥，可由 root 拥有、服务可读。
3. 将 `fintwind-stun.service` 放到 `/etc/systemd/system/`，`systemctl daemon-reload`
   后 `enable --now fintwind-stun`。单元用 `LogsDirectory` 创建
   `/var/log/fintwind-stun`，在 `ProtectSystem=strict` 下可写日志。
4. 在云防火墙与主机防火墙放行 UDP 18762 入站。STUN 不需要 TCP 18762 或 TLS 5349，
   也不放行任何中继端口段。

## 验证

用 STUN 客户端向 `stun:<服务域名>:18762` 发 Binding 请求，确认返回
XOR-MAPPED-ADDRESS。确认 TURN Allocate 被忽略，服务器不分配中继端口。
本目录不保存验证记录或密钥。

## 许可

本目录的配置文件为 Fintwind 原创，随仓库根 `LICENSE` 采用 GPL-3.0。coturn 为
BSD-3-Clause 许可的开源项目（[coturn/coturn](https://github.com/coturn/coturn)），
分发 coturn 时保留其许可证。
