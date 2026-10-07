# relay 部署（UCloud 165.154.64.159 / relay.fintwind.xyz）

一次性部署步骤。服务器事实核对：Debian 12.7、2 核、1.9G 内存、59G 盘。
来源：[remote-control-m0-freeze.md](../../docs/remote-control-m0-freeze.md)
部署锚点节与本目录的 [Caddyfile](Caddyfile)、
[fintwind-relay.service](fintwind-relay.service)。

## 0. 前置（云控制台 + DNS）

1. UCloud 安全组：开放 `80/tcp`、`443/tcp`（80 供 ACME HTTP-01）；22 保持现状。
2. DNS：`relay.fintwind.xyz` A 记录 → `165.154.64.159`。

## 1. 服务器准备

```sh
# 少量 swap：2G 内存机器防编译与瞬时峰值 OOM
fallocate -l 2G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile
echo '/swapfile none swap sw 0 0' >> /etc/fstab

apt update && apt install -y caddy build-essential git curl
# Rust（stable）——以 rustup 官方脚本安装
curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal
```

## 2. 构建与安装

```sh
git clone <repo> /opt/fintwind && cd /opt/fintwind
source $HOME/.cargo/env
cargo build --locked --release -p fintwind-relay
install -m 755 target/release/fintwind-relay /usr/local/bin/fintwind-relay
```

（2 核 2G 编译 relay 及其依赖约数分钟；swap 已就位。）

## 3. 服务账号与数据目录

```sh
useradd --system --home /var/lib/fintwind-relay --shell /usr/sbin/nologin fintwind-relay
mkdir -p /var/lib/fintwind-relay
chown fintwind-relay:fintwind-relay /var/lib/fintwind-relay
```

## 4. systemd 与 Caddy

（无引导步骤：host 与设备的身份在各自首次启动时用密钥自动注册，relay
不做任何账号与密码。）

```sh
cp deploy/relay/fintwind-relay.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now fintwind-relay

cp deploy/relay/Caddyfile /etc/caddy/Caddyfile
systemctl reload caddy   # Origin CA 证书见 docs/remote-control-m0-freeze.md 部署锚点
```

## 5. 验收

```sh
curl https://relay.fintwind.xyz/api/health      # {"ok":true}
ss -ltnp | grep 8090                             # 仅 127.0.0.1 监听
journalctl -u fintwind-relay -n 20               # 只有 audit 行；无 token/正文
```

边界自查（对应架构文档"relay 只见密文与身份元数据"）：

- 数据库只有 hosts、devices、pairing_links、pairing_nonces 四张表——
  `sqlite3 relay.sqlite3 ".tables"` 核对；没有任何密码哈希或凭据材料
- 日志不记录 token、nonce、URL、认证头
- `strict` 防护下 relay 进程只写自己的数据目录

## 6. 已知限制（诚实清单）

- 单机单 relay；达到连接/队列上限会明确拒绝并告警（日志），扩容是独立决策。
- relay 重启 = 所有端点重连并从各自对端恢复状态；投递队列不落盘是设计而非缺陷。
- 证书续期依赖 80/443 畅通；Caddy 自动处理，但防火墙变更后要复核。
