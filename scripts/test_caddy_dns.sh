#!/usr/bin/env bash
set -euo pipefail

image=${1:-nx-boss-caddy:cloudflare-a8737d0}
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
printf '%s\n' 'cfut_123456789012345678901234567890123456789012345678' > "$test_dir/dns_key"
chmod 600 "$test_dir/dns_key"

for profile in default custom; do
  if [ "$profile" = default ]; then
    scanner_http=20447 scanner_https=20448 admin_https=443
    upstream_scanner=10447 upstream_admin=10448
  else
    scanner_http=21447 scanner_https=21448 admin_https=8443
    upstream_scanner=11447 upstream_admin=11448
  fi

  docker run --rm \
    --user "$(id -u):$(id -g)" \
    --cap-drop ALL \
    --cap-add NET_BIND_SERVICE \
    --env NX_BOSS_SCANNER_HTTP_PORT="$scanner_http" \
    --env NX_BOSS_SCANNER_HTTPS_PORT="$scanner_https" \
    --env NX_BOSS_ADMIN_HTTPS_PORT="$admin_https" \
    --env NX_BOSS_UPSTREAM_SCANNER_PORT="$upstream_scanner" \
    --env NX_BOSS_UPSTREAM_ADMIN_PORT="$upstream_admin" \
    --entrypoint /bin/sh \
    --mount "type=bind,src=$PWD/deploy/Caddyfile,dst=/etc/caddy/Caddyfile,readonly" \
    --mount "type=bind,src=$test_dir/dns_key,dst=/run/secrets/cloudflare_api_token,readonly" \
    "$image" /usr/local/bin/start-caddy.sh adapt --validate --config /etc/caddy/Caddyfile --adapter caddyfile \
    > "$test_dir/caddy-$profile.log" 2>&1 || { cat "$test_dir/caddy-$profile.log"; exit 1; }

  if grep -Fq 'cfut_123456789012345678901234567890123456789012345678' "$test_dir/caddy-$profile.log"; then
    echo 'Caddy exposed the API token in its output' >&2
    exit 1
  fi
done

echo 'Caddy DNS-01 config accepts cfut_ tokens and custom ports'
