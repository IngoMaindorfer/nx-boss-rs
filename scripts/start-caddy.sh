#!/bin/sh
set -eu

CF_API_TOKEN=$(cat /run/secrets/cloudflare_api_token)
export CF_API_TOKEN
exec caddy "$@"
