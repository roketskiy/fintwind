# PWA 独立发布：最小迁移

> **当前发布分工**：用户在暂停后恢复时明确“Cloudflare的部署还是我自己来吧”。
> 因此下面较早的 MCP 代发布授权不再适用。代理只交付验收包、冻结目录与哈希，
> 用户自行在现有 Pages 项目发布；不改官网或向 UCloud 保存 Pages 凭据。

不增加域名购买或 UCloud 实例。中继保留 `relay.fintwind.xyz`，手机页面使用
`remote.fintwind.xyz`，托管在独立 Cloudflare Pages 项目 `fintwind-remote`。
官网 `fintwind.xyz` 不变。UCloud 不保存 Pages 发布凭据，也不再提供手机代码。
项目创建页确认的地址是 `https://fintwind-remote.pages.dev`；尚待上传和激活，
不能仅凭项目名就标记独立入口已上线。

## 用户的一次性操作

2026-10-08：用户已配置 Cloudflare MCP 和 skill，并授权代理在验收通过后自行
部署页面。以下手动上传步骤只作备用，不再要求用户上传 ZIP。项目入口已确认；
代理使用实际可用的工具与权限发布，不索取或记录 Cloudflare 密钥。

2026-10-09：用户进一步明确“仅使用 Cloudflare MCP”作为代理发布入口，不通过
内置浏览器发布。当前服务连接成功，但本会话工具目录未暴露该 MCP，需区分
已配置、可调用与已部署三个状态。恢复可调用后发布，不要求用户手动上传。

1. Cloudflare → Workers & Pages，创建 Pages Direct Upload 项目
   `fintwind-remote`。页面包由下列命令产生，不需提供密码或 API Token。
2. 在项目 Custom domains 中添加 `remote.fintwind.xyz`，按提示创建或确认 DNS。
   必须先在 Pages 关联域名，不能只手工添加 CNAME。
3. 若需要备用手动发布，验收通过后上传页面包；后续更新在同一 Pages 项目
   创建新 deployment，不重新创建项目。当前项目地址已经确认，无需重复提供。

## 构建与发布

在主仓根目录执行：

```powershell
pwsh -NoProfile -File deploy/pwa/prepare-pages.ps1
```

脚本重建 WASM 和页面，检查正式 bundle、CSP、安装资源、WASM 字节与来源状态，
再冻结到唯一目录。生成根目录正确的 zip 与全资产 SHA-256/源码记录到
`C:/Users/Public/Temp/opencode/fintwind-pages/`。不修改服务器，不等于行为验收。
后续部署使用报告中的 `deploymentDirectory`；不要另行重建未验收的 `dist`。

如已完成真实本地链路，交付时使用该轮候选目录与 `summary.json`，不再重建：

```powershell
pwsh -NoProfile -File deploy/pwa/prepare-pages.ps1 -SiteDirectory '<该轮候选目录>' -AcceptanceReport '<该轮 summary.json>'
```

脚本拒绝失败或未清理的报告，并将每个页面资产与验收哈希核对。基础链路报告
只证明自己的覆盖范围；打包不会将其改成全量验收或公网验收。
不要再使用旧的 `deploy-pwa.ps1` 同源上传流程；保留它仅供迁移前历史核对。

若使用 CI，使用独立 PWA 仓库与独立 Pages 发布秘密；不能把该秘密放到中继
服务器或复用中继 SSH 密钥。Cloudflare 账号和域名管理员仍有更高权限，属于
保留的信任边界。仅两个子域名不构成独立账户级隔离。

## 切换顺序

1. 用候选协议版本完成隔离黑盒、WASM、手机控制与代码审查。
2. 先上传 Pages 候选包。确认独立入口、CSP、缓存、WASM/glue 同构。
3. 更新中继，并配置精确 Origin 白名单；只添加实际使用的 Pages 生产来源，
   不允许 `*.pages.dev`、任意预览来源或开发端口。
4. 二维码改为独立 PWA 的 `/pair#...`，中继地址仍是 `relay.fintwind.xyz`。
5. 用全新身份及隔离工作区完成公网 Pages → relay → host → daemon 验收。
6. 验收成功后应用 `../relay/Caddyfile-isolated`，使旧来源停止提供手机代码。
   不在旧中继来源保留可解密/操作的旧页面或 Service Worker。

协议升级会断开旧客户端；明示版本不兼容，不自动降级。若新发布失败，应
回滚同一协议的已验证 Pages deployment；跨协议回滚必须同时核对端点与中继。

浏览器设备身份按来源隔离，不能从旧 `relay.fintwind.xyz` 自动搬到新来源。
新入口首次使用需要在电脑前重新配对；不要导出私钥或通过 URL 搬运身份。
验收后在电脑设备页移除旧入口对应的设备授权，并清除旧来源的站点数据。
由 `pages.dev` 再切换到 `remote.fintwind.xyz` 时同样需要重新配对，因此建议
正式使用前完成自定义域名，避免让普通用户重复迁移。

## 验收与回滚

- `/pair` 返回页面，WASM/glue 加载成功；拒绝 iframe 嵌套、危险外链与业务缓存。
- 注册、XX 校验码、本机确认、KK 重连、发送与结果查询、审批、撤销真实有效。
- Service Worker 仅静态壳；旧 worker 更新后不混用新旧协议，业务和待提交输入
  不进入 Cache API、IndexedDB 或 localStorage（设备身份是独立例外）。
- 保存 deployment ID、zip SHA-256、host/relay 版本及脱敏验收记录。
- 用 Pages 的 rollback 恢复已知可用 deployment；重验 WASM 与协议，不放宽授权。

发布隔离未落实、密码审查未完成或真实链路未通过时，不开放真实工作区外部试用。
