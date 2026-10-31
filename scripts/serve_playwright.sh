#!/usr/bin/env bash
set -euo pipefail

test_dir=$(mktemp -d)
server_pid=
proxy_pid=
cleanup() {
  if [ -n "$proxy_pid" ]; then kill "$proxy_pid" 2>/dev/null || true; wait "$proxy_pid" 2>/dev/null || true; fi
  if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
  rm -f /tmp/nx-boss-browser-storage.json
  rm -f /tmp/nx-boss-browser-passkey.json
  rm -rf "$test_dir"
}
trap cleanup EXIT INT TERM
mkdir -p "$test_dir/scans"
cat > "$test_dir/config.yaml" <<EOF
jobs:
  BrowserTest:
    output_path: $test_dir/scans
    color: '#2196F3'
  SecondJob:
    output_path: $test_dir/scans
    color: '#E91E63'
  ThirdJob:
    output_path: $test_dir/scans
    color: '#9C27B0'
EOF

python3 - "$test_dir/auth-pepper" <<'PY'
import os, pathlib, sys
path = pathlib.Path(sys.argv[1]); path.write_bytes(os.urandom(32)); path.chmod(0o600)
PY
export NX_BOSS_AUTH_PEPPER_FILE="$test_dir/auth-pepper"
export NX_BOSS_ADMIN_DOMAIN=localhost
export NX_BOSS_ADMIN_HTTPS_PORT=12449

./target/debug/nx-boss-rs --config "$test_dir/config.yaml" --host 127.0.0.1 --port 12447 --admin-host 127.0.0.1 --admin-port 12448 >"$test_dir/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 60); do
  if curl --noproxy '*' -fsS http://127.0.0.1:12447/health >/dev/null 2>&1; then break; fi
  if ! kill -0 "$server_pid" 2>/dev/null; then cat "$test_dir/server.log"; exit 1; fi
  sleep 1
done
curl --noproxy '*' -fsS http://127.0.0.1:12447/health >/dev/null
bash scripts/test_scan.sh 127.0.0.1:12447 "$test_dir/scans" >"$test_dir/seed.log" 2>&1 || { cat "$test_dir/seed.log"; exit 1; }
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$test_dir/key.pem" -out "$test_dir/cert.pem" -days 1 -subj /CN=localhost -addext subjectAltName=DNS:localhost >/dev/null 2>&1
python3 scripts/test_https_proxy.py "$test_dir/cert.pem" "$test_dir/key.pem" 12449 12448 localhost:12449 >"$test_dir/proxy.log" 2>&1 &
proxy_pid=$!
for _ in $(seq 1 60); do
  if curl --noproxy '*' -kfsS https://localhost:12449/setup >/dev/null 2>&1; then break; fi
  sleep .2
done
python3 scripts/bootstrap_browser_test.py "$test_dir" /tmp/nx-boss-browser-storage.json /tmp/nx-boss-browser-passkey.json
wait "$server_pid"
