# Fintwind mobile

React and TypeScript single-page client. Vite emits static files into `dist/`. Components come from the official shadcn/ui registry, using the Base UI Maia preset. Icons reuse the repository's embedded SVG assets. Runtime icon libraries are not required.

## Build

Run these commands with a suitable command timeout. Use the configured network proxy for installation.

```sh
bun install --frozen-lockfile
bun run build
```

From the repository root, `bun run mobile:build` runs type checking and the production build. No server or dev watcher is started. Neither command is a functional test.

Only deploy `dist/`, never this source directory or `node_modules`. Personal bootstrap and migration scripts stay outside Git. For an existing relay, use `scripts/deploy-mobile-ui.ps1 -ConfigPath <private-config-path> -ExpectedSiteHash <current-site-sha256>` with a suitable command timeout after explicit deployment approval. Keep the configuration outside the checkout; use `deploy/remote-control/mobile-ui.example.json` for its schema. Read the hash from the configured Caddy site before deploying. The publisher refuses to replace a site that changed after that inspection. Use `-CheckOnly` for offline configuration validation.

The frontend publisher builds and uploads only static files plus the reviewed site configuration. Releases live under `/var/www/fintwind-mobile-ui-releases/`. The site uses `/var/www/fintwind-mobile-current`, a symlink switched by rename. It keeps the original bootstrap directory, previous releases and the previous site configuration. Previous hashed assets remain available to already-open tabs. It reloads Caddy but never restarts the relay. On activation failure it restores the previous pointer and site configuration. Successful deployments print the backup path and verify static file hashes and service state, not endpoint behavior.

## Ownership

- `src/lib/remote-client.ts` owns connection state, pairing, request tracking, bounded conditional caching and reconnects.
- `src/lib/remote-crypto.ts` implements the existing encrypted protocol. `remote-validation.ts` validates incoming data.
- `src/hooks/use-remote.ts` owns workspace and session selection, reads and user operations.
- `src/app.tsx` composes the session-list and conversation screens. Message and approval components live in `src/components`.
- `src/components/ui` contains shadcn component source. Theme tokens and responsive layout live in `src/index.css`.

Lists and conversations are separate phone screens. Browser Back returns to the session list. The list remains mounted to retain scroll position. Drafts stay in memory per session. Chat content is not persisted. A page reload can recover pairing and unresolved operation IDs from per-tab session storage, but does not restore drafts or queue messages offline.

## Security and deployment

The existing end-to-end encryption and restricted desktop commands remain in use. Mutation commands are never automatically retried. A timeout or disconnect leaves the operation blocked until a later read confirms current state. Pairing secrets never enter application navigation URLs, logging or rendered markup.

Markdown does not execute HTML. External images are not loaded. The UI uses system fonts and same-origin scripts. No analytics, service worker or third-party CDN is used. Caddy permits inline style attributes for Base UI layout variables, but scripts and style elements remain same-origin only.

The user prohibited tests for this work. Build and static review do not establish browser, phone-keyboard, touch, accessibility or real remote-control behavior.
