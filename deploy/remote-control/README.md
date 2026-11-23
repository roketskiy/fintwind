# Mobile remote-control deployment

This directory holds the direct-connect signaling service (`signal/`), the
STUN-only address discovery service (`stun/`), the shared Caddy site template, the
service installer, and the static-web publisher. The legacy business relay
(`relay/`) is retained as upstream source reference only; its routes are retired
from the Caddy template and the publisher no longer depends on it. Keep host
addresses, SSH identity paths, DNS administration settings, and migration
snapshots in a private directory outside the checkout. Never commit registration
keys, signing secrets, pairing links, live environment files, or deployment
archives.

## Install the signaling and STUN services

The public site and WebSocket use TCP 18761, and STUN uses UDP 18762. Caddy
manages the site's TLS certificate; its HTTP-01 renewal challenge also requires
inbound TCP 80 when a certificate is issued or renewed.

Run `install-signal-stun.sh` as root once per host before the first frontend
release, and again after changing the signal source or unit. Pass a Node 20+
linux-x64 tarball URL or local path; add `--with-stun` to also install the
STUN-only service (the distribution coturn package must be present first).

The installer prepares a complete new version in its own release directory under
`/opt/fintwind-signal/releases/<id>` (Node runtime and locked dependencies, install
scripts disabled), then atomically repoints the `current` symlinks and restarts
`fintwind-signal.service`. A running process is never overwritten, the health
check reflects the new code, and the previous release is kept for rollback. It
writes a default environment file only when absent. It runs on the remote host and
does not assume a local proxy: curl and npm honor the process environment
(`HTTP_PROXY`/`HTTPS_PROXY`), and `FINTWIND_INSTALL_PROXY` overrides the proxy only
when explicitly set (empty forces a direct connection). Downloads and the whole
`npm ci` have bounded timeouts. It does not publish the frontend, reload Caddy, or
touch the legacy relay. See `signal/README.md` and `stun/README.md` for configuration
and boundaries.

## Publish the mobile webpage

1. Copy `mobile-ui.example.json` to a private location outside the repository.
   Replace its example values and restrict the file to the deployment operator.
2. Inspect the configured Caddy site on the intended host. Record its current
   SHA256 and confirm that Caddy and the signaling service are healthy.
3. Run with a suitable command timeout:

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
whose hash changed, atomically changes the frontend symlink and the Caddy site,
retains rollback files, and reloads only Caddy. It checks that the signaling
service stays healthy and its PID is unchanged; it does not restart signaling or
run remote-control feature tests. Only deploy the built `mobile/dist` files, not
preview fixtures or source files.

## Legacy relay reference

`relay/` vendors the upstream Alma relay described in `UPSTREAM.md`. It is no
longer the deployment target: the Caddy template serves the mobile page and
`/signal/v2` on one origin and returns 404 for the former `/agent`, `/c/*`, and
`/v1/admission/*` routes. The bundled administration CLI (`revoke`, `list`,
`allow`) and admission policy apply only to that legacy service and are not used
by the direct-connect path.
