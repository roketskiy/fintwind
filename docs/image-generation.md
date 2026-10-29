# Image model configuration

## Failure cases

- An image output capability without `type: image` must not be offered as a working image model.
- A chat package must not be used to construct an image model.
- Repairing an image model must not change its provider's chat package, endpoint, credentials, other models, or unrelated configuration.
- Existing native model packages, types, headers, body and settings must survive repair.
- Unknown protocols and text-only models, including URL-returning aliases, must not be guessed from their names.
- Repeated migration must be idempotent; JSONC comments must survive.
- Disabling, removing or changing an image model must not leave an automatically generated native override active.
- A failed write or concurrently modified config must not destroy the original file.
- Model discovery must use the selected workspace's authenticated OpenCode server, never models.dev as an availability fallback.
- Failed or superseded discovery must not expose stale models as available.
- An unavailable configured default must be shown as unavailable, not as a selectable working model.

## Validation boundary

Use isolated configuration, data and cache directories, environment whitelisting, random loopback ports, and owned Windows Job Objects. Only the loopback fixture may receive generation requests. Never use production credentials or stop existing processes. Record a repeatable result artifact.

## Implementation

- Model protocol defaults to inheriting the provider, with per-model overrides for Chat Completions, Responses, Anthropic, Google, OpenAI Images and Google Images.
- An explicit model package wins over automatic image inference. Unknown native packages are preserved.
- Known legacy image protocols are converted to native V2 providers before adding `type: image` and their image entrypoint. OpenCode replaces an entire legacy provider when a native provider with the same ID exists; partial overlays would lose its endpoint, credentials and chat roster.
- A migration first writes a uniquely named `.before-image-v2-*.bak` beside the original configuration. JSONC and unrelated root fields are preserved; moved provider blocks are reformatted.
- Explicit text output takes precedence over catalog inference. Neither a model's name nor a `-url` suffix establishes image protocol compatibility.
- The picker uses the selected workspace's `/api/model?types=image` via the daemon, with generation guards and no metadata fallback. Unavailable defaults remain visible but cannot be selected as working models.
- All configuration work and RPCs run off the UI thread. Opening General settings or saving providers refreshes the image inventory.

## Single isolated check

```sh
cargo build --locked -p fintwind-daemon -p fintwind-client --bin fintwind-daemon --example image_models_blackbox
python scripts/provider-credentials-check.py target/debug/examples/image_models_blackbox.exe target/debug/fintwind-daemon.exe <absolute-opencode.exe>
```

The existing bounded runner supplies an environment whitelist, independent data directories, random ports, and owned Job Objects. The example checks migration, preservation, model-level override/inheritance, the real OpenCode image inventory and deletion. It makes no image-generation request. The result is `target/provider-credentials-check/<run-id>/result.json`.
