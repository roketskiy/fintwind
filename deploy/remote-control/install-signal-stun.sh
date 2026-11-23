#!/bin/sh
# Install or update the Fintwind direct-connect signaling and STUN-only services.
#
# This prepares the services that the frontend publisher (publish-mobile-ui.sh)
# expects to be healthy. It does not publish the mobile page, reload Caddy, or
# touch the legacy relay.
#
# Update behavior: a complete new version is prepared in its own release
# directory (Node runtime and locked dependencies included) before anything is
# switched. Only after the new version is fully built does the script atomically
# repoint the `current` symlinks and restart the service, so a running process is
# never overwritten and the health check always reflects the new code. The
# previous release directory is kept for rollback.
#
# Usage: sh install-signal-stun.sh <node-tarball-url-or-path> [--with-stun]
#   Pass an official Node 20+ linux-x64 tarball (URL or local path). STUN uses the
#   distribution coturn package and is only installed when --with-stun is given.
#
# Network: this runs on the remote host and does not assume a local proxy. curl
# and npm honor the process environment (HTTP_PROXY / HTTPS_PROXY). Set
# FINTWIND_INSTALL_PROXY to override the proxy, or to an empty value to force a
# direct connection. Downloads and the whole `npm ci` have bounded timeouts.
#
# Run as root. Review every step before running on a live host.
set -eu
umask 077

node_source=${1:?Node 20+ linux-x64 tarball URL or path required}
with_stun=0
for arg in "$@"; do
    case "$arg" in --with-stun) with_stun=1 ;; esac
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
signal_src="$script_dir/signal"
stun_src="$script_dir/stun"

signal_user=fintwind-signal
signal_root=/opt/fintwind-signal
signal_releases="$signal_root/releases"
signal_current="$signal_root/current"
signal_node_current="$signal_root/node/current"
signal_env=/etc/fintwind-signal.env

# Bounded network settings. This script runs on the remote host, so it does not
# assume a local proxy exists. By default curl and npm honor the process
# environment (HTTP_PROXY / HTTPS_PROXY / ALL_PROXY and their lowercase forms).
# FINTWIND_INSTALL_PROXY overrides the proxy only when explicitly set; set it to
# an empty value to force a direct connection even if the environment has one.
# The proxy is applied through the environment so no shell quoting or globbing of
# proxy arguments is needed.
if [ "${FINTWIND_INSTALL_PROXY+set}" = set ]; then
    if [ -n "$FINTWIND_INSTALL_PROXY" ]; then
        HTTP_PROXY=$FINTWIND_INSTALL_PROXY
        HTTPS_PROXY=$FINTWIND_INSTALL_PROXY
        http_proxy=$FINTWIND_INSTALL_PROXY
        https_proxy=$FINTWIND_INSTALL_PROXY
        export HTTP_PROXY HTTPS_PROXY http_proxy https_proxy
    else
        unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy
    fi
fi
download_timeout=${FINTWIND_INSTALL_DOWNLOAD_TIMEOUT:-180}
# Per-request fetch timeout for npm, and a separate outer wall-clock limit for the
# whole `npm ci` command (fetch timeout alone does not bound the full install).
npm_fetch_timeout=${FINTWIND_INSTALL_NPM_TIMEOUT:-120000}
npm_total_timeout=${FINTWIND_INSTALL_NPM_TOTAL_TIMEOUT:-300}

id=$(date -u +%Y%m%dT%H%M%SZ)-$$
release="$signal_releases/$id"
node_tarball=$(mktemp)
cleanup() {
    rm -f "$node_tarball"
}
trap cleanup EXIT HUP INT TERM

# --- signaling service -----------------------------------------------------

if ! id "$signal_user" >/dev/null 2>&1; then
    useradd --system --home-dir "$signal_root" --shell /usr/sbin/nologin "$signal_user"
fi

install -d -m 755 -o root -g root "$signal_root" "$signal_releases" "$signal_root/node"

# Prepare the complete new version in its own directory. Nothing live is touched
# until the symlinks are repointed below.
install -d -m 755 -o root -g root "$release"
install -d -m 755 -o root -g root "$release/node"

# Node runtime into the new release. Accept a local tarball path or an https URL.
case "$node_source" in
    https://*) curl -fsSL --connect-timeout 15 --max-time "$download_timeout" -o "$node_tarball" "$node_source" ;;
    *) cp "$node_source" "$node_tarball" ;;
esac
tar -xzf "$node_tarball" -C "$release/node" --strip-components=1
[ -x "$release/node/bin/node" ]

# Application source and locked dependencies, installed into the new release.
# Install scripts are disabled. The outer `timeout` bounds the whole install;
# the fetch flags bound each individual request and its retries.
cp "$signal_src/index.mjs" "$signal_src/protocol.mjs" "$signal_src/room.mjs" "$signal_src/limits.mjs" "$signal_src/package.json" "$signal_src/package-lock.json" "$release/"
(cd "$release" && timeout "$npm_total_timeout" "$release/node/bin/node" "$release/node/bin/npm" ci \
    --ignore-scripts --omit=dev --no-audit --no-fund \
    --fetch-retries=2 --fetch-retry-mintimeout=2000 --fetch-retry-maxtimeout=10000 \
    --fetch-timeout="$npm_fetch_timeout")
chown -R root:"$signal_user" "$release"
chmod -R u=rwX,go=rX "$release"

# Environment file. Create only if absent so an operator's values are kept.
if [ ! -f "$signal_env" ]; then
    cat > "$signal_env" <<'EOF'
HOST=127.0.0.1
PORT=8790
MAX_ROOMS=64
MAX_CONNECTIONS=256
EOF
    chmod 640 "$signal_env"
    chown root:"$signal_user" "$signal_env"
fi

install -m 644 "$signal_src/fintwind-signal.service" /etc/systemd/system/fintwind-signal.service
systemctl daemon-reload
systemctl enable fintwind-signal.service

# Atomically repoint both symlinks to the fully prepared new version, then
# restart so the running process is the new code. `enable --now` would not
# restart an already-active service, so restart is explicit.
ln -s "$release" "$signal_root/.current-$id"
mv -Tf "$signal_root/.current-$id" "$signal_current"
ln -s "$release/node" "$signal_root/node/.current-$id"
mv -Tf "$signal_root/node/.current-$id" "$signal_node_current"

systemctl restart fintwind-signal.service
attempt=0
until curl -fsS --max-time 2 http://127.0.0.1:8790/healthz > /dev/null 2>&1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 10 ]; then
        echo 'Signaling health check failed.' >&2
        exit 1
    fi
    sleep 1
done
systemctl is-active --quiet fintwind-signal.service

echo "Signaling release: $release"
echo "Previous releases kept under $signal_releases for rollback."

# --- STUN-only service (optional) ------------------------------------------

if [ "$with_stun" -eq 1 ]; then
    if ! command -v turnserver >/dev/null 2>&1; then
        echo 'coturn (turnserver) is not installed. Install the distribution coturn package first.' >&2
        exit 1
    fi
    if ! id fintwind-stun >/dev/null 2>&1; then
        useradd --system --home-dir /var/lib/fintwind-stun --shell /usr/sbin/nologin fintwind-stun
    fi
    install -d -m 755 -o root -g root /etc/fintwind-stun
    install -m 644 "$stun_src/turnserver.conf" /etc/fintwind-stun/turnserver.conf
    install -m 644 "$stun_src/fintwind-stun.service" /etc/systemd/system/fintwind-stun.service
    systemctl daemon-reload
    systemctl enable fintwind-stun.service
    # coturn reads its config at start; restart so an updated config takes effect.
    systemctl restart fintwind-stun.service
    systemctl is-active --quiet fintwind-stun.service
    echo 'STUN installed. Open UDP 18762 inbound in the cloud and host firewalls.'
fi

echo 'Signaling service installed and healthy. Publish the frontend with scripts/deploy-mobile-ui.ps1.'
