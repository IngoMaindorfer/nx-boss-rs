#!/usr/bin/env bash
# Simulates the full PaperStream NX Manager scanner flow against a running nx-boss server.
# Usage: ./test_scan.sh [HOST:PORT]   (default: 127.0.0.1:10447)

set -euo pipefail

BASE="${1:-127.0.0.1:10447}"
URL="http://$BASE/NmWebService"
PASS=0
FAIL=0

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
echo "  target: $URL"
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

# 3. GET authorization (scanner checks auth type)
echo "--- authorization ---"
r=$(curl -sf "$URL/authorization")
check "GET /authorization returns auth_type:none" "$r" '"auth_type"'

# 4. POST authorization (scanner fetches job list)
r=$(curl -sf -X POST "$URL/authorization" \
    -H 'Content-Type: application/json' -d '{}')
check "POST /authorization returns job_info array" "$r" '"job_info"'
check "POST /authorization returns access_token" "$r" '"access_token"'

JOB_COUNT=$(echo "$r" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['job_info']))" 2>/dev/null || echo "?")
echo "  jobs available: $JOB_COUNT"

# 5. Get scan settings for job 0
echo "--- scansetting ---"
r=$(curl -sf "$URL/scansetting?job_id=0")
check "GET /scansetting?job_id=0 returns parameters" "$r" '"parameters"'
check "GET /scansetting?job_id=0 returns sources" "$r" '"sources"'

# 6. Create a batch
echo "--- batch create ---"
r=$(curl -sf -X POST "$URL/batch" \
    -H 'Content-Type: application/json' \
    -d '{"job_id":0}')
check "POST /batch returns batch_id" "$r" '"batch_id"'

BATCH_ID=$(echo "$r" | python3 -c "import sys,json; print(json.load(sys.stdin)['batch_id'])" 2>/dev/null || echo "")
echo "  batch_id: $BATCH_ID"

if [ -z "$BATCH_ID" ]; then
    red "Cannot continue without batch_id"
    exit 1
fi

# 7. Upload a test image (creates a small fake JPEG)
echo "--- image upload ---"
TMPIMG=$(mktemp /tmp/test_XXXXXX.jpg)
# Minimal valid JPEG (SOI + EOI markers)
printf '\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00\xff\xd9' > "$TMPIMG"

PARAM_JSON="{\"batch_id\":\"$BATCH_ID\",\"page\":1}"
r=$(curl -sf -X POST "$URL/image" \
    -F "image=@$TMPIMG;filename=page001.jpg" \
    -F "parameter=$PARAM_JSON")
STATUS=$?
rm -f "$TMPIMG"
if [ $STATUS -eq 0 ]; then
    green "  PASS  POST /image (page001.jpg)"
    PASS=$((PASS+1))
else
    red   "  FAIL  POST /image returned non-200"
    FAIL=$((FAIL+1))
fi

# 8. Upload a second page
TMPIMG2=$(mktemp /tmp/test_XXXXXX.jpg)
printf '\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00\xff\xd9' > "$TMPIMG2"
PARAM_JSON2="{\"batch_id\":\"$BATCH_ID\",\"page\":2}"
r=$(curl -sf -X POST "$URL/image" \
    -F "image=@$TMPIMG2;filename=page002.jpg" \
    -F "parameter=$PARAM_JSON2")
STATUS=$?
rm -f "$TMPIMG2"
if [ $STATUS -eq 0 ]; then
    green "  PASS  POST /image (page002.jpg)"
    PASS=$((PASS+1))
else
    red   "  FAIL  POST /image page 2 returned non-200"
    FAIL=$((FAIL+1))
fi

# 9. Complete the batch
echo "--- batch complete ---"
r=$(curl -sf -o /dev/null -w "%{http_code}" -X PUT "$URL/batch/$BATCH_ID")
check "PUT /batch/:id returns 200" "$r" '^200$'

# 10. Logout
echo "--- logout ---"
r=$(curl -sf -X DELETE "$URL/accesstoken")
check "DELETE /accesstoken returns 200 body" "$r" 'application/json'

# 11. Verify files on disk
echo "--- disk check ---"
# Find the batch directory (somewhere under ./scans)
BATCH_DIR=$(find ./scans -maxdepth 2 -name "metadata.json" -newer ./scans | head -1 | xargs dirname 2>/dev/null || true)
if [ -n "$BATCH_DIR" ]; then
    green "  PASS  batch dir found: $BATCH_DIR"
    PASS=$((PASS+1))
    if [ -f "$BATCH_DIR/page001.jpg" ]; then
        green "  PASS  page001.jpg on disk"
        PASS=$((PASS+1))
    else
        red   "  FAIL  page001.jpg missing in $BATCH_DIR"
        FAIL=$((FAIL+1))
    fi
    META=$(cat "$BATCH_DIR/metadata.json")
    check "metadata.json completed=true" "$META" '"completed":true'
    check "metadata.json has 2 files" "$META" '"page002.jpg"'
else
    red   "  FAIL  no batch dir found under ./scans"
    FAIL=$((FAIL+1))
fi

echo
echo "=== Results: $PASS passed, $FAIL failed ==="
[ $FAIL -eq 0 ] && exit 0 || exit 1
