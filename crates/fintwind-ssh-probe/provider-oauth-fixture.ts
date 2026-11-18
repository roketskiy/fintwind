import { Plugin } from "@opencode/plugin/effect"
import { Effect } from "effect"

export default Plugin.define({
  id: "fintwind.provider-oauth-fixture",
  effect: (ctx) => Effect.gen(function* () {
    const integration = ctx.integration
    const endpoint = String(ctx.options.endpoint)
    const method = { id: "fintwind-fixture", type: "oauth" as const, label: "Sign in to SSH Fixture" }
    yield* integration.transform((editor) => {
      editor.update("openai", (item) => (item.name = "SSH OAuth Fixture"))
      editor.method.update({
        integrationID: "openai",
        method,
        authorize: () => Effect.tryPromise(async () => {
          const response = await fetch(endpoint + "/device", { method: "POST" })
          if (!response.ok) throw new Error("Could not start fixture authorization")
          const { device, url } = await response.json()
          return {
            mode: "auto" as const,
            url,
            instructions: "Approve the isolated fixture in your browser.",
            callback: Effect.tryPromise(async () => {
              for (let i = 0; i < 360; i++) {
                const reply = await fetch(endpoint + "/token", { method: "POST", body: device,
                  signal: AbortSignal.timeout(3000) })
                if (reply.ok && reply.status !== 202) {
                  const { access, refresh } = await reply.json()
                  return { type: "oauth" as const, methodID: method.id, access, refresh, expires: Date.now() + 3600000 }
                }
                if (reply.status !== 202) throw new Error("Fixture token exchange refused")
                await new Promise((resolve) => setTimeout(resolve, 500))
              }
              throw new Error("Fixture approval timed out")
            }),
          }
        }),
      })
    })
    yield* integration.reload()
    yield* ctx.session.hook("http.request", ({ request }) => Effect.gen(function* () {
      const connection = yield* integration.connection.active("openai")
      const credential = connection ? yield* integration.connection.resolve(connection).pipe(Effect.orDie) : undefined
      if (credential?.type === "oauth") request.headers.set("Authorization", `Bearer ${credential.access}`)
    }), { providerID: "fixture" })
  }),
})
