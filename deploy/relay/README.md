# relay 部署（UCloud 165.154.64.159 / relay.fintwind.xyz）

> 2026-10-08 迁移说明：下文第 7 节保留同源部署的历史路径，不是新的生产方案。
> 手机页面迁往 Cloudflare Pages，见 [独立发布流程](../pwa/README.md)。
> 页面与公网链路验收通过后使用 `Caddyfile-isolated`；不能提前关掉旧入口，
> 也不能把旧同源部署称为已经完成发布隔离。

一次性部署步骤。服务器事实核对：Debian 12.7、2 核、1.9G 内存、59G 盘。
来源：[remote-control-m0-freeze.md](../../docs/remote-control-m0-freeze.md)
部署锚点节与本目录的 [Caddyfile](Caddyfile)、
[fintwind-relay.service](fintwind-relay.service)。

## 0. 前置（云控制台 + DNS）

1. UCloud 安全组：开放 `443/tcp`；Origin CA 模式不依赖 ACME，80 可关闭；22 保持现状。
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
- Origin CA 证书按部署锚点手动轮换；Cloudflare 必须使用 Full (strict)，不关闭证书验证。

### 独立页面迁移后的来源限速

候选 `Caddyfile-isolated` 仅信任 Cloudflare 官方网段，以 `CF-Connecting-IP` 解析
真实客户端，并向 loopback relay 覆盖为一个规范 `X-Forwarded-For` IP。未知直连
来源不信任客户端自带代理头。默认裸 `reverse_proxy` 也会忽略未受信 XFF，不能
把它描述为已证实的公网伪造旁路；但它会使同一 Cloudflare 边缘的客户端共享
来源预算，所以候选配置明确限定信任来源，不直接保留输入 XFF 的最左段。

2026-10-09 只读核对：服务器 Caddy 为 2.11.7，支持 strict 代理解析；网段来源为
`https://www.cloudflare.com/ips-v4`、`https://www.cloudflare.com/ips-v6`。候选配置
尚未应用，真实代理头与限速验收仍须通过公网链路；语法校验不替代该验收。

## 7. PWA 同域托管（M4 起）

PWA 构建产物由本机的 Caddy 直接托管，与 relay 同域（`https://relay.fintwind.xyz`）：

- `Caddyfile`：`/api/*` 与 `/ws` 反代 127.0.0.1:8090；其余路径服务
  `/var/www/pwa`（SPA fallback 到 index.html，`/pair#…` 扫码直达）。
- hashed assets 长缓存；index.html 与 sw.js `no-cache`，发布即时生效。
- relay 的浏览器 Origin 白名单在 unit 的
  `FINTWIND_RELAY_ORIGINS=https://relay.fintwind.xyz`（同源部署只有这一项；
  开发期临时 origin 走 drop-in override，不得进生产 unit）。

发布 PWA（本机执行，需 wasm-pack 与 bun）：

```sh
pwsh -File deploy/pwa/deploy-pwa.ps1
```

脚本做：`bun run build` → 打包 dist → scp → 解压到 /var/www/pwa → 公网 200 验证。

wasm 桥以静态脚本加载，不经过打包器：

- 产物用 `bun run build:wasm` 生成（fintwind-pwa/scripts/build-wasm.mjs），
  内部执行 `wasm-pack build crates/fintwind-remote-crypto-wasm --target no-modules
  --out-dir ../../fintwind-pwa/public/wasm --out-name fintwind-crypto`，
  并把生成的 d.ts 同步到 fintwind-pwa/src/types/wasm-crypto.d.ts。
- index.html 先加载 /wasm/fintwind-crypto.js，再由 /publish.js 把绑定发布为
  `window.__wasmCrypto`（glue 的 `let` 声明不进 window，模块代码也访问不到；
  publish.js 放在 public/ 根下，不放 public/wasm，避免被 build:wasm 重建清掉）。
- src/crypto-bridge.ts 的调用全部经由这个全局对象。
- 曾经用 --target web 让 rollup 处理 glue，结果 dev 正常、生产在握手阶段
  panic（"null pointer passed to rust"）：同一代码两条行为路径。不要再改回
  打包器路径；消费 `self` 的 wasm 方法同样禁止（用 `&mut self` + `Option::take`），
  否则 wasm-bindgen 0.2.125+ 的 glue 会在参数编组时 panic。

历史版本曾通过 controllerchange 自动刷新。当前方案已改为用户明确确认更新，
禁止自动 reload 清除待提交输入；迁往独立来源仍需重新配对与清理旧站点数据。
变更 Caddyfile / unit 后需要：

```sh
systemctl daemon-reload && systemctl reload caddy && systemctl restart fintwind-relay
```
