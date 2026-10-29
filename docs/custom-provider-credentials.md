# Custom provider credentials

Custom provider IDs and model configuration remain unchanged. Literal API keys
are stored through OpenCode's credential API. Configuration references such as
`{env:NAME}` and `{file:path}` remain configuration references.

## Failure cases to verify

- An unavailable server or failed credential write must not remove the old key.
- A credential must be read back successfully before removing its inline key.
- A failed config write must leave the on-disk provider usable and allow retry.
- Repeated saves must not create duplicate accounts or change provider IDs.
- Migration must not replace another account or remove unknown model options.
- Editing or deleting a provider during an asynchronous save must not restore
  an older UI snapshot; subsequent saves must run in order.
- Removing a provider definition must not delete shared OpenCode credentials.
- Model discovery and probes must use the selected credential without storing
  it in the UI roster or configuration.

Validation is limited to compilation and one isolated black-box run. No user
configuration, production credentials, or existing processes are test targets.

## Implementation

- The existing provider/model editor and compatible configuration fields remain.
- Keys are migrated on loading the provider settings, when OpenCode is available.
- Credential creation is verified by reading it back before updating configuration.
- Key replacement creates and verifies a new account before removing superseded
  accounts in Fintwind's own credential namespace. Other accounts are preserved.
- API-key account choices come from OpenCode's sanitized integration connections.
- Explicit logout removes stored accounts; deleting a provider definition does not.
- A replacement key is a separate input draft until Save is pressed.
- Probes resolve the currently selected credential only for the background request.
  They do not retain it in the UI roster or configuration.

## Focused check

```sh
cargo build --locked -p fintwind-daemon -p fintwind-client --bin fintwind-daemon --example custom_provider_credentials_blackbox
python scripts/provider-credentials-check.py target/debug/examples/custom_provider_credentials_blackbox.exe target/debug/fintwind-daemon.exe <absolute-opencode.exe>
```

The runner uses an environment whitelist, explicit daemon/OpenCode data paths,
random loopback ports, and owned Windows Job Objects. The total runtime is
bounded at 150 seconds. It writes a secret-free `result.json` under
`target/provider-credentials-check/<run-id>/`.
