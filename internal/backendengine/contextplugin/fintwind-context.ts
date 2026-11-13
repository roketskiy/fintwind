// fintwind.context: OpenCode 服务端插件。订阅 session.context 钩子,在每次
// 主模型请求组装后把各分类的 token 估算写入 FINTWIND_CONTEXT_DIR 下的快照
// 文件,供 Fintwind daemon 生成上下文成分面板。估算口径见
// docs/context-composition.md;分类拆解参考 dsh-context(Apache-2.0)。
// 仅当引擎由 Fintwind 启动(注入 FINTWIND_CONTEXT_DIR)时才生效,直接使用
// OpenCode CLI 时本插件不做任何事。
import { mkdir, rename, writeFile } from "node:fs/promises"
import path from "node:path"

const CHARS_PER_TOKEN = 4
const READ_TOOLS = new Set(["read", "read_file"])

// blockTokens 按文档口径估算一个内容块:每 4 个 ASCII 字符计 1 token
// (逐块向上取整),每个非 ASCII 字符计 1 token。
function blockTokens(text: string): number {
  if (!text) return 0
  let ascii = 0
  let nonAscii = 0
  for (const ch of text) {
    if ((ch.codePointAt(0) ?? 0) < 128) ascii += 1
    else nonAscii += 1
  }
  return Math.ceil(ascii / CHARS_PER_TOKEN) + nonAscii
}

// jsonText 序列化任意值,用于工具输入参数与输入结构。
function jsonText(value: unknown): string {
  if (value === undefined || value === null) return ""
  if (typeof value === "string") return value
  try {
    return JSON.stringify(value) ?? ""
  } catch {
    return ""
  }
}

// resultText 把工具结果值规范化为文本。
function resultText(value: unknown): string {
  if (value == null) return ""
  if (typeof value === "string") return value
  if (Array.isArray(value)) return value.map((item) => resultText(item)).join("")
  if (typeof value === "object") {
    const record = value as Record<string, unknown>
    if (typeof record.text === "string") return record.text
    return jsonText(value)
  }
  return String(value)
}

export default {
  id: "fintwind.context",
  async setup(ctx: { session: { hook: (name: string, callback: (event: any) => Promise<void>) => unknown } }) {
    const dir = process.env.FINTWIND_CONTEXT_DIR
    if (!dir) return
    ctx.session.hook("context", async (event: any) => {
      try {
        const totals = {
          system: 0,
          tools: 0,
          messages: 0,
          reasoning: 0,
          toolCalls: 0,
          files: 0,
          toolResults: 0,
          memory: 0,
        }
        let opaqueReasoning = false
        let opaqueMemory = false
        // 系统部分与其指令来源(键名与文本都计入系统与规则)。
        for (const part of event.system ?? []) {
          totals.system += blockTokens(part?.text ?? "")
          const instructions = part?.metadata?.experimental?.instructions
          if (Array.isArray(instructions)) {
            for (const instruction of instructions) {
              totals.system += blockTokens(`${instruction?.key ?? ""}\n${instruction?.text ?? ""}`)
            }
          }
        }
        // 工具定义:名称、描述与输入结构。
        for (const [name, tool] of Object.entries(event.tools ?? {})) {
          totals.tools += blockTokens(`${name}\n${tool?.description ?? ""}\n${jsonText(tool?.input)}`)
        }
        for (const message of event.messages ?? []) {
          const role = message?.role
          const parts = typeof message?.content === "string"
            ? [{ type: "text", text: message.content }]
            : Array.isArray(message?.content)
              ? message.content
              : []
          for (const part of parts) {
            switch (part?.type) {
              case "text": {
                if (role === "system") {
                  totals.system += blockTokens(part.text ?? "")
                } else if (typeof part.metadata?.filename === "string" && part.metadata.filename) {
                  totals.files += blockTokens(part.text ?? "")
                } else {
                  totals.messages += blockTokens(part.text ?? "")
                }
                break
              }
              case "media": {
                // 文本附件计入文件;图像等媒体按文档不计。
                const text = part.media?.text
                if (typeof text === "string") totals.files += blockTokens(text)
                break
              }
              case "reasoning":
                // 加密推理不可测量,只标记不透明。
                if (part.encrypted !== undefined) opaqueReasoning = true
                else totals.reasoning += blockTokens(part.text ?? "")
                break
              case "tool-call":
                totals.toolCalls += blockTokens(`${part.name ?? ""}\n${jsonText(part.input)}`)
                break
              case "tool-result": {
                const text = resultText(part.result)
                if (READ_TOOLS.has(String(part.name ?? ""))) totals.files += blockTokens(text)
                else totals.toolResults += blockTokens(text)
                break
              }
              case "compaction":
                // 加密或缺失摘要的原生压缩状态不可测量。
                if (part.encrypted !== undefined || part.text == null) opaqueMemory = true
                else totals.memory += blockTokens(part.text)
                break
            }
          }
        }
        const payload = {
          version: 1,
          sessionID: event.sessionID,
          model: `${event.model?.providerID ?? ""}/${event.model?.id ?? ""}`,
          agent: event.agent,
          opaqueReasoning,
          opaqueMemory,
          ...totals,
        }
        await mkdir(dir, { recursive: true })
        const target = path.join(dir, `${event.sessionID}.json`)
        const staging = `${target}.${process.pid}.tmp`
        await writeFile(staging, JSON.stringify(payload), "utf8")
        await rename(staging, target)
      } catch {
        // 观测失败不得影响请求派发。
      }
    })
  },
}
