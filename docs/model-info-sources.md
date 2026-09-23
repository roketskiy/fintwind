# 模型信息的来源与新增模型流程

新增一个模型（或一个模型版本）只改一处参考表，但要经过「查来源 → 定档位 →
写表 → 生成请求变体 → 验证」这条链路。本文把来源优先级和完整写法固化下来，
避免每次重新摸索。价格、知识截止日期、发布日期不写进任何参考表。

新增模型时先读第二节的完整流程，再按第三节的速查清单收尾。

## 一、信息来源与查证顺序

模型名称、上下文、输出上限、模态、思考档位和默认档分布在不同来源。按
「及时性 → 权威性」分四层使用，跳层采信会把转售商的错误字段写进配置。

### 1. models.dev API（第一站，最及时）

```
https://models.dev/api.json
```

每个模型的两个关键字段：

- `reasoning`：是否是推理模型。
- `reasoning_options`：档位形态，三种类型：
  - `{"type":"effort","values":["none","low","medium","high",...]}` — 档位枚举
  - `{"type":"toggle"}` — 只能开/关
  - `{"type":"budget_tokens","min":1024}` — token 预算

名称、`limit.context`、`limit.output`、`modalities` 也取自这里，按 id 填空缺、
不覆盖已有值，不手改 `temp/model-cache`。

一份文件几 MB，别整份读进上下文。只查厂商自己的条目，用脚本按 id 取字段：

```powershell
$catalog = Invoke-RestMethod -Uri 'https://models.dev/api.json'
$m = $catalog.openai.models.'gpt-6-sol'          # 或 anthropic / xai / google
$m.name; $m.reasoning_options | ConvertTo-Json -Depth 10
$m.limit | ConvertTo-Json -Depth 10; $m.modalities | ConvertTo-Json -Depth 10
```

#### 聚合数据不能单独信

models.dev 是社区聚合，转售商条目互相矛盾是常态。实测（223 家供应商、
215 家带 reasoning 字段）的例子：同一个 `claude-sonnet-4-6`，转售商标
`effort: low/medium/high/max`（把 OpenAI 的词套到 Claude 上），另一家标
`toggle + effort + budget_tokens` 三种全给。

规则：**只读厂商自己的 provider 条目**（xAI 用 `xai`，不用转售商的改价或
改模态副本）。转售商副本只做交叉印证，不作为依据。

### 2. 厂商官方文档（最终权威）

models.dev 不标默认档，也不说明能否关闭思考——这两项必须回厂商文档：

| 厂商 | 查什么 |
|---|---|
| Anthropic | extended/adaptive thinking：`thinking.type`、budget 范围、`effort` 默认档、能否关闭思考 |
| OpenAI | reasoning effort：哪些模型支持哪些档（`none`/`low`/`medium`/`high`/`xhigh`/`max`）与默认档 |
| Google | Gemini `thinkingConfig`：`thinkingLevel` vs `thinkingBudget`、隐式思考何时可关 |
| xAI | grok 系列的 reasoning 参数与默认档 |

「网关会不会接受某个值」文档同样不会告诉你，只能实测：配置被 OpenCode
接受、目录能列出档位，不等同于真实供应商已验证。

### 3. 交叉验证（可选，两条免费 JSON）

- **OpenRouter** `GET /api/v1/models` 与模型详情的 `supported_parameters`
  （含 `reasoning_effort` / `reasoning_effort_values`）。
- **LiteLLM** `model_prices_and_context_window.json` 的
  `supports_reasoning_effort` / `supported_reasoning_effort_values`。

与 models.dev 不同源，三方一致时可信度很高；不一致时以厂商文档为准。

### 4. 形态贴 OpenCode V2 协议

档位最终要经 OpenCode V2 规范化（`mode_key`、`variants`）。落表前对一下
opencode v2 仓库里 provider 对 `reasoning_options` 的解释，确认 `tiered` /
`toggle` / budget 三类如何映射到 `reasoningEffort` / `thinking.type` /
`thinkingConfig`。参考表只记「档位是什么」，不记「怎么发请求」——后者由
`provider_thinking.rs` 按 API 格式推导，见第 4 步。

---

## 二、新增模型：完整流程

### 涉及的文件

| 文件 | 作用 | 何时改 |
|---|---|---|
| `models_thinking_modes.json` | 唯一的参考表：档位、默认档、预算。`include_str!` 编进二进制 | 每加一个模型/版本 |
| `crates/fintwind-protocol/src/thinking_modes.rs` | 解析参考表并提供 `find()` 匹配 | 一般不改 |
| `crates/fintwind-client/src/provider_thinking.rs` | 把档位翻译成各家请求选项（`mode_options`） | 引入新映射/新格式时 |
| `crates/fintwind-client/src/opencode_config.rs` | 保存供应商时调用 `fill_model_variants` 写入 `variants`/`options` | 一般不改 |
| `crates/fintwind-core/src/model_catalog.rs` | 从服务器目录读已有档位、贴标签、定默认 | 一般不改 |
| `src/ui/mod.rs` → `MODEL_COMPANY_MARKS` | 新模型家族的品牌图标 | 仅当是新公司 |

`fallback_models()` 故意返回空：目录来自运行中的 OpenCode，静态兜底会让不可用
模型看起来可选。所以**不需要**往任何静态列表里加模型。

### 第 1 步：取候选数据

按第一节的脚本从 models.dev 取厂商自己的条目，得到：

- 显示名（`name`）——参考表的 `name` 要和它对得上；
- `reasoning_options`——候选档位与 `mode_key`；
- `limit.context` / `limit.output` / `modalities`——这三项由 models.dev 元数据表
  按 id 自动补齐（见 `crates/fintwind-client/src/models_dev.rs`），不写进参考表。

### 第 2 步：定默认档与能否关闭思考

回厂商文档确认两件 models.dev 不标的事：

- 默认档是哪个（写进唯一的 `is_default: true`）；
- 能否关思考。不能关的（如 Claude Opus 5.5 自适应思考始终开启）就**不要**
  写 `nothinking` 档；能关的才加，并按规范映射成 `reasoningEffort: none` /
  `thinking.type: disabled` 等。

### 第 3 步：写 `models_thinking_modes.json`

每条格式：

```json
{
  "name": "GPT-6 Sol",
  "thinking_modes": [
    {
      "mode_key": "medium",
      "name_zh": "中",
      "name_en": "Medium",
      "mode_type": "tiered",
      "thinking_budget_tokens": null,
      "is_default": true,
      "vendor_label": null
    }
  ]
}
```

字段规则：

- **一个版本一条**，不从上一个版本整段复制——相邻版本的档位可以不同。
- `mode_key` 用 models.dev `reasoning_options` 的原值（`low`/`medium`/`high`/
  `xhigh`/`max`/`none`，toggle 类用 `nothinking`/`thinking`/`adaptive`）。
- 只用 **一个** `is_default: true`。
- `thinking_budget_tokens` 保持 `null`，除非厂商明确给出预算数值。
- `name_zh`/`name_en` 与既有条目用词保持一致（低/中/高/极高/最高，关闭/开启/
  自适应思考）。
- `mode_type` 与 `vendor_label` 目前**不被 Rust 解析**（`ThinkingMode` 只反序列化
  `mode_key`/`name_en`/`name_zh`/`thinking_budget_tokens`/`is_default`，serde 忽略
  未知字段）。它们随表保存，仅供人读，保持既有取值即可。
- 价格、知识截止日期、发布日期**不进这张表**。

#### 匹配规则（决定 `name` 怎么写）

`thinking_modes::find(id, name)` 的查找顺序：

1. 取 id 最后一个 `/` 之后的部分（如 `openai/gpt-6-sol` → `gpt-6-sol`），
   归一化（只留 ASCII 字母数字并转小写）后查表；
2. 查不到时用传入的显示名 `name` 归一化再查；
3. 仍查不到且 id 以 `claude` 开头，去掉前缀再查（覆盖 `Opus 4.7` 这种不带
   `Claude` 的条目标名）。

**只做归一化后的精确匹配，不做家族猜测**，所以 `Pro`/`Flash`/带日期版本
必须各有自己的条目。举例：

- `gpt-6-sol` → `gpt6sol`，与 `"GPT-6 Sol"` → `gpt6sol` 精确命中；
- `claude-opus-5-5` → `claudeopus55`，与 `"Claude Opus 5.5"` 命中；
- `claude-opus-4-7` → 直接查不到，靠第 3 步去掉 `claude` 命中 `Opus 4.7`。

新增条目后跑一次匹配测试即可确认（见第 5 步）。

### 第 4 步：确认档位 → 请求选项的映射

参考表里的 `mode_key` 由 `provider_thinking.rs::mode_options` 按**供应商 API
格式**翻译成请求选项：

| `mode_key` | OpenAI / o 系 / Responses | Anthropic（Opus 5.5 类） | Anthropic（旧，token 预算） | Gemini |
|---|---|---|---|---|
| `nothinking` | `reasoningEffort: none` | 不写此档 | `thinking.type: disabled` | `thinkingBudget: 0` |
| `low`…`max` | `reasoningEffort: <key>` | `effort: <key>` | `thinking.type: enabled` + `budgetTokens` | `thinkingLevel: <key>` |
| `adaptive` | — | 等价于默认，可省 | `thinking.type: adaptive` | — |

- **Anthropic 自适应 + 可调 effort 的新模型**（如 Opus 5.5）：思考不能关、
  也没有手动预算，档位写成 `{"effort": "<key>"}`。Fintwind 写配置时用 `effort`
  这个键，OpenCode 的 anthropic provider 会把它 lower 成 `output_config.effort`。
- **Anthropic 旧模型**：走 `thinking.type: enabled` + `budgetTokens`，预算取
  `thinking_budget_tokens`，缺失时用预设（low 1024 / medium 8192 /
  max·xhigh·extended 24576 / 其余 16384）。
- **OpenAI 系**（`gpt`/`o1`/`o3`/`o4` 开头，或 Responses 格式）：`reasoningEffort`。
- **Gemini**：`thinkingConfig` 的 `thinkingBudget` 或 `thinkingLevel`。
- **其余兼容网关**：`reasoningEffort` 加上 `thinking`（deepseek/glm/kimi）或
  `enable_thinking`（其他），属推断而非厂商声明。

只有在出现**新格式**时才需要改这里，并补一个单元测试（参考
`provider_thinking.rs` 里的 `opus_55_uses_anthropic_effort_without_disabling_adaptive_thinking`）。

### 第 5 步：验证

```sh
cargo test --locked -p fintwind-protocol   # 参考表解析与 find() 匹配
cargo test --locked -p fintwind-client     # 档位 → 请求选项的映射
git diff --check                           # JSON/空白无误
```

- 新模型名建议在 `thinking_modes.rs` 的匹配测试里加一行断言，防止 `name`
  拼写与 id 不归一化一致。
- 若走了新的映射分支，`provider_thinking.rs` 必须补测试。
- 端到端只在需要时做：在 dev watcher 管理、已签名的应用里保存一次供应商，
  确认选择器列出新档位且默认档正确。

### 第 6 步：让改动生效

`fill_model_variants` 在**保存供应商时**执行，且参考表是 `include_str!` 编进
二进制的：

1. 参考表改动需要**重新编译**（dev watcher 会自动重编重启）；
2. 应用里**重新保存一次供应商**——只补缺失的档位，用户已手改的档位不覆盖；
3. OpenCode 重新读取配置后，菜单/选择器才显示新档。

---

## 三、速查清单

1. 脚本从 models.dev 取厂商条目 → 记下 `name` 与 `reasoning_options`。
2. 厂商文档定默认档与能否关思考。
3. （可选）OpenRouter / LiteLLM 交叉印证。
4. 在 `models_thinking_modes.json` 加一个版本一条，`mode_key` 用原值，只标一个
   `is_default`，预算与 `vendor_label` 保持 `null` 除非厂商给值。
5. 确认 `mode_key` 能被 `provider_thinking.rs` 正确映射；新格式才改代码补测试。
6. 跑 protocol + client 测试与 `git diff --check`。
7. 重编 → 重新保存供应商 → 确认档位与默认档。
8. 新公司才动 `src/ui/mod.rs` 的 `MODEL_COMPANY_MARKS`。
