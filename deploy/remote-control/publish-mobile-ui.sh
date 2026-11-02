#!/bin/sh
set -eu
umask 077
stage=${1:?Release directory required}
id=${2:?Release identifier required}
expected=${3:?Expected Caddy site hash required}
site=${4:?Reviewed Caddy site path required}
case "$site" in /etc/caddy/*.caddy) ;; *) echo 'Invalid Caddy site path.' >&2; exit 1;; esac
site_name=${site#/etc/caddy/}
case "$site_name" in *[!a-zA-Z0-9._-]*) echo 'Invalid Caddy site filename.' >&2; exit 1;; esac
current=/var/www/fintwind-mobile-current
releases=/var/www/fintwind-mobile-ui-releases
release=$releases/$id
case "$id" in *[!a-zA-Z0-9-]*|'') echo 'Invalid release identifier.' >&2; exit 1;; esac
case "$expected" in *[!a-f0-9]*|'') echo 'Invalid site hash.' >&2; exit 1;; esac
[ "${#expected}" -eq 64 ]
[ -f /opt/fintwind-alma-relay/multiuser-v1 ]
[ -f "$stage/mobile/index.html" ]
[ -f "$stage/mobile/licenses/shadcn.txt" ]
[ "$(sha256sum "$site" | cut -d ' ' -f 1)" = "$expected" ]
systemctl is-active --quiet fintwind-alma-relay.service
systemctl is-active --quiet caddy.service
relay_pid=$(systemctl show fintwind-alma-relay.service -p MainPID --value)
(cd "$stage" && sha256sum -c SHA256SUMS)
if [ -e "$current" ] && [ ! -L "$current" ]; then echo 'Refusing to replace a non-symlink frontend pointer.' >&2; exit 1; fi
previous=
if [ -L "$current" ]; then
    previous=$(readlink "$current")
    case "$previous" in "$releases"/*) ;; *) echo 'Unknown frontend pointer target.' >&2; exit 1;; esac
fi
[ ! -e "$release" ]
install -d -m 755 -o root -g root "$releases"
mkdir -m 700 "$release"
mkdir -m 755 "$release/mobile"
cp -a "$stage/mobile/." "$release/mobile/"
if [ -n "$previous" ] && [ -d "$previous/assets" ]; then
    cp -an "$previous/assets/." "$release/mobile/assets/"
fi
chown -R root:root "$release/mobile"
chmod -R u=rwX,go=rX "$release/mobile"
cp -p "$site" "$release/site.before"
printf '%s\n' "$previous" > "$release/previous-pointer"
sed 's|root \* /var/www/fintwind-mobile$|root * /var/www/fintwind-mobile-current|' "$stage/Caddyfile" > "$release/site.next"
grep -Fq 'root * /var/www/fintwind-mobile-current' "$release/site.next"
chmod 755 "$release"
chmod 600 "$release/site.before" "$release/site.next" "$release/previous-pointer"
pointer_switched=0
site_switched=0
temporary_link=/var/www/.fintwind-mobile-$id
temporary_site=/etc/caddy/.fintwind-mobile-$id
rollback() {
    trap - EXIT HUP INT TERM
    failed=0
    if [ "$pointer_switched" -eq 1 ]; then
        if [ -n "$previous" ]; then
            ln -s "$previous" "$temporary_link" && mv -Tf "$temporary_link" "$current" || failed=1
        else
            rm -f "$current" || failed=1
        fi
    fi
    if [ "$site_switched" -eq 1 ]; then
        install -m 644 "$release/site.before" "$temporary_site" && mv -Tf "$temporary_site" "$site" || failed=1
        systemctl reload caddy.service || failed=1
    fi
    echo "Frontend deployment failed; rollback status=$failed; backup=$release" >&2
    exit 1
}
trap rollback EXIT HUP INT TERM
ln -s "$release/mobile" "$temporary_link"
mv -Tf "$temporary_link" "$current"
pointer_switched=1
install -m 644 "$release/site.next" "$temporary_site"
site_switched=1
mv -Tf "$temporary_site" "$site"
systemctl reload caddy.service
systemctl is-active --quiet caddy.service
[ "$(systemctl show fintwind-alma-relay.service -p MainPID --value)" = "$relay_pid" ]
systemctl is-active --quiet fintwind-alma-relay.service
(cd "$current" && grep '  mobile/' "$stage/SHA256SUMS" | sed 's|  mobile/|  |' | sha256sum -c -)
trap - EXIT HUP INT TERM
printf 'Frontend release: %s\nPrevious pointer: %s\nBackup: %s\nRelay PID unchanged: %s\n' "$id" "$previous" "$release" "$relay_pid"
systemctl show fintwind-alma-relay.service caddy.service -p Id -p ActiveState -p SubState -p MainPID
stat -c '%a %U %G %n' "$current/index.html" "$current/assets" "$site"
