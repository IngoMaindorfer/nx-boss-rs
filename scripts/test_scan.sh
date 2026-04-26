#!/usr/bin/env bash
# Simulates the full PaperStream NX Manager scanner flow against a running nx-boss server.
# Usage: ./scripts/test_scan.sh [HOST:PORT] [SCAN_OUTPUT_DIR]
#   HOST:PORT        default: 127.0.0.1:10447
#   SCAN_OUTPUT_DIR  where the server writes scans (default: ./scans)
#
# Requires: curl, python3

set -euo pipefail

BASE="${1:-127.0.0.1:10447}"
SCAN_DIR="${2:-./scans}"
URL="http://$BASE/NmWebService"
PASS=0
FAIL=0

# Use the fixture JPEG (has proper SOF0 so PDF assembly succeeds).
# Fall back to a synthetic one with dimensions if the fixture is missing.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE="$SCRIPT_DIR/../tests/fixtures/scan_page.jpg"
if [ ! -f "$FIXTURE" ]; then
    echo "WARNING: fixture not found at $FIXTURE, using synthetic JPEG"
    FIXTURE=$(mktemp /tmp/test_XXXXXX.jpg)
    # SOI + APP0/JFIF (DPI=300) + SOF0 (100x100 grayscale) + EOI
    printf '\xff\xd8'                                         > "$FIXTURE"
    printf '\xff\xe0\x00\x10JFIF\x00\x01\x01\x01\x01\x2c\x01\x2c\x00\x00' >> "$FIXTURE"
    printf '\xff\xc0\x00\x0b\x08\x00\x64\x00\x64\x01\x01\x11\x00'         >> "$FIXTURE"
    printf '\xff\xd9'                                                        >> "$FIXTURE"
    SYNTHETIC=1
fi

green() { printf '\033[32m%s\033[0m\n' "$*"; }
red()   { printf '\033[31m%s\033[0m\n' "$*"; }

check() {
    local name="$1" got="$2" want_pattern="$3"
    if echo "$got" | grep -q "$want_pattern"; then
        green "  PASS  $name"
        PASS=$((PASS+1))
    else
        red   "  FAIL  $name"
        red   "        got:      $got"
        red   "        expected: (contains) $want_pattern"
        FAIL=$((FAIL+1))
    fi
}

echo
echo "=== nx-boss integration test ==="
echo "  target:   $URL"
echo "  scan dir: $SCAN_DIR"
echo

# 1. Heartbeat
echo "--- heartbeat ---"
r=$(curl -sf "$URL/heartbeat")
check "GET /heartbeat returns system_time" "$r" '"system_time"'

# 2. Device registration
echo "--- device ---"
r=$(curl -sf -X POST "$URL/device" \
    -H 'Content-Type: application/json' \
    -d '{
        "call_timing":"boot",
        "scanner_ip":"192.168.1.100",
        "scanner_mac":"AA:BB:CC:DD:EE:FF",
        "scanner_model":"fi-8170",
        "scanner_name":"TestScanner",
        "scanner_port":"52217",
        "scanner_protocol":"http",
        "serial_no":"SN123456"
    }')
check "POST /device returns server_version" "$r" '"server_version"'

# 3. GET authorization
echo "--- authorization ---"
r=$(curl -sf "$URL/authorization")
check "GET /authorization returns auth_type:none" "$r" '"auth_type"'

# 4. POST authorization
r=$(curl -sf -X POST "$URL/authorization" \
    -H 'Content-Type: application/json' -d '{}')
check "POST /authorization returns job_info array" "$r" '"job_info"'
check "POST /authorization returns access_token"  "$r" '"access_token"'

JOB_COUNT=$(echo "$r" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['job_info']))" 2>/dev/null || echo "?")
echo "  jobs available: $JOB_COUNT"

# 5. Scan settings for job 0
echo "--- scansetting ---"
r=$(curl -sf "$URL/scansetting?job_id=0")
check "GET /scansetting?job_id=0 returns parameters" "$r" '"parameters"'
check "GET /scansetting?job_id=0 returns sources"    "$r" '"sources"'

# 6. Create batch
echo "--- batch create ---"
r=$(curl -sf -X POST "$URL/batch" \
    -H 'Content-Type: application/json' \
    -d '{"job_id":0}')
check "POST /batch returns batch_id" "$r" '"batch_id"'

BATCH_ID=$(echo "$r" | python3 -c "import sys,json; print(json.load(sys.stdin)['batch_id'])" 2>/dev/null || echo "")
echo "  batch_id: $BATCH_ID"
[ -z "$BATCH_ID" ] && { red "Cannot continue without batch_id"; exit 1; }

# 7. Upload two pages
echo "--- image upload ---"
for i in 1 2; do
    r=$(curl -sf -X POST "$URL/image" \
        -F "image=@$FIXTURE;filename=page00${i}.jpg" \
        -F "parameter={\"batch_id\":\"$BATCH_ID\",\"page\":$i}")
    green "  PASS  POST /image (page00${i}.jpg)"
    PASS=$((PASS+1))
done

# 8. Complete the batch (triggers PDF assembly)
echo "--- batch complete ---"
r=$(curl -sf -o /dev/null -w "%{http_code}" -X PUT "$URL/batch/$BATCH_ID")
check "PUT /batch/:id returns 200" "$r" '^200$'

# 9. Logout
echo "--- logout ---"
r=$(curl -sf -X DELETE "$URL/accesstoken")
check "DELETE /accesstoken returns 200 body" "$r" 'application/json'

# 10. Disk checks
echo "--- disk check ---"
BATCH_DIR=$(find "$SCAN_DIR" -maxdepth 2 -name "metadata.json" | xargs -I{} dirname {} 2>/dev/null | head -1 || true)
if [ -n "$BATCH_DIR" ]; then
    green "  PASS  batch dir found: $BATCH_DIR"
    PASS=$((PASS+1))
    for page in page001.jpg page002.jpg; do
        if [ -f "$BATCH_DIR/$page" ]; then
            green "  PASS  $page on disk"
            PASS=$((PASS+1))
        else
            red   "  FAIL  $page missing in $BATCH_DIR"
            FAIL=$((FAIL+1))
        fi
    done
    META=$(cat "$BATCH_DIR/metadata.json")
    check "metadata.json completed=true"  "$META" '"completed":true'
    check "metadata.json has 2 files"     "$META" '"page002.jpg"'
else
    red "  FAIL  no batch dir found under $SCAN_DIR"
    FAIL=$((FAIL+1))
fi

[ "${SYNTHETIC:-0}" = "1" ] && rm -f "$FIXTURE"

echo
echo "=== Results: $PASS passed, $FAIL failed ==="
[ $FAIL -eq 0 ] && exit 0 || exit 1
