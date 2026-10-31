# fintwind update gateway

One cached JSON for the desktop update check, served at
`https://fintwind.roketskiy.top/v1/latest`. It fixes the GitHub API
rate-limit problem: the desktop used to page
`api.github.com/repos/.../releases` anonymously on every launch
(60 req/hour per IP, shared with the whole NAT), and now does one
small GET against this Worker instead.

## How it works

- `GET /v1/latest?current=0.2.19` → `200` JSON or `204` when already current.
- `GET /v1/latest` → `200` JSON (newest stable, no comparison).
- `GET /healthz` → `200` JSON.
- The Worker pages the GitHub releases API with a **server-side**
  `GITHUB_TOKEN`, picks the newest stable release by
  `(published_at, id)` — the same publication-order rule as the desktop —
  and caches the answer at the edge for 5 minutes
  (`s-maxage=300, stale-while-revalidate=60`).
- On upstream failure it serves the stale cache instead of failing every
  client at once; with no cache it returns `502`, and the desktop falls
  back to `api.github.com` directly.

## Deploy (Cloudflare CLI)

DNS for `roketskiy.top` is already on Cloudflare; `wrangler.jsonc`
routes `fintwind.roketskiy.top/*` to this Worker.

```sh
cd workers/update-gateway
bunx wrangler@latest login
bunx wrangler@latest secret put GITHUB_TOKEN
# A token with public-repo read is enough (fine-grained PAT, no scopes
# needed for public data); it only raises the server-side rate limit.
bunx wrangler@latest types
bunx wrangler@latest deploy
```

Verify:

```sh
curl -i 'https://fintwind.roketskiy.top/v1/latest?current=0.0.0'
curl -i https://fintwind.roketskiy.top/healthz
```

Expected: `200` with `{ version, notes, url, assets: [...] }`
(`sha256` is bare hex, or `null` when the release published no digest).

## Client contract

`src/update.rs` calls this gateway first (`UPDATE_GATEWAY_URL`,
overridable at runtime with `FINTWIND_UPDATE_GATEWAY` for tests) and
falls back to `api.github.com` when the gateway is unreachable. The
gateway response shape matches the desktop `ReleaseInfo` struct, so no
second round trip is needed to pick the installer.
