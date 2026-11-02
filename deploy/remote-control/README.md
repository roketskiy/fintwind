# Mobile relay deployment

This directory contains reusable relay source, the service unit, a Caddy site
template, and a static-web publisher. Personal bootstrap and one-time migration
scripts are not part of the repository. Keep host addresses, SSH identity paths,
DNS administration settings, deployment-owner markers, and migration snapshots
in a private directory outside the checkout. Never commit registration keys,
signing secrets, pairing links, live environment files, or deployment archives.

## Publish the mobile webpage

1. Copy `mobile-ui.example.json` to a private location outside the repository.
   Replace its example values and restrict the file to the deployment operator.
2. Inspect the configured Caddy site on the intended host. Record its current
   SHA256 and confirm that Caddy and the relay service are healthy.
3. After explicit deployment approval, run with a suitable command timeout:

   ```powershell
   pwsh -NoProfile -File scripts/deploy-mobile-ui.ps1 -ConfigPath <private-config-path> -ExpectedSiteHash <current-site-sha256>
   ```

Use `-CheckOnly` to validate configuration without building, contacting the
server, or changing files on the server. The script rejects configuration inside
the checkout. It does not print the configuration or copy it into the release.

`Caddyfile` uses `{$FINTWIND_MOBILE_DOMAIN}` as a template placeholder. The local
publisher substitutes the configured domain in the staged release. Do not copy
the unresolved template onto a live server without configuring its variable.
The publisher verifies the archive and file hashes, refuses to overwrite a site
whose hash changed, atomically changes the frontend symlink, retains rollback
files, and reloads only Caddy. It does not restart the relay or run remote-control
feature tests. Only deploy the built `mobile/dist` files, not preview fixtures or
source files.

## Relay installation and maintenance

Use the pinned upstream source described in `UPSTREAM.md` and inspect the relay
and service unit before adapting them for a new installation. Generate secrets on
the server and keep them in an operator-controlled environment file. Do not
reuse another installation's private bootstrap or signing key. Personal migration
scripts are intentionally excluded rather than generalized into a tool that
could replace an unrelated service.

The bundled administration CLI supports `revoke <installation-id>`, `list`, and
`allow <installation-id>`. Installation IDs are public SHA256 identifiers, not
pairing secrets. Run the CLI using the intended installation's Node runtime and
`admin.mjs` path. Its policy file must remain root-owned and service-readable.
Confirm that an interrupted administration command has stopped before removing
its exact lock file. Missing or invalid policy must fail closed. Reinstalling can
create a new identity; retain IP and global admission limits as well.
