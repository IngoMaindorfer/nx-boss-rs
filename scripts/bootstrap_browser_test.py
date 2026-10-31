#!/usr/bin/env python3
"""Create a disposable admin account and Playwright storage state."""

import base64
import hashlib
import hmac
import html
import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


root = Path(sys.argv[1])
token = (root / "admin-setup-token").read_text()
origin = "https://127.0.0.1:12449"
headers = {"Origin": origin, "Host": "127.0.0.1:12449", "X-Forwarded-Proto": "https"}


def post(path, data):
    request = urllib.request.Request(
        "http://127.0.0.1:12448" + path,
        data=urllib.parse.urlencode(data).encode(),
        headers=headers,
    )
    return urllib.request.urlopen(request, timeout=10)


response = post("/setup/prepare", {"token": token, "username": "browser-admin"})
page = response.read().decode()
match = re.search(r"<code>([A-Z2-7]+)</code>", page)
assert match, "setup page did not return a TOTP secret"
manual_secret = html.unescape(match.group(1))
secret = base64.b32decode(manual_secret + "=" * ((-len(manual_secret)) % 8))
counter = int(time.time()) // 30
digest = hmac.new(secret, counter.to_bytes(8, "big"), hashlib.sha1).digest()
offset = digest[-1] & 15
code = str((int.from_bytes(digest[offset:offset + 4], "big") & 0x7fffffff) % 1_000_000).zfill(6)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


request = urllib.request.Request(
    "http://127.0.0.1:12448/setup/complete",
    data=urllib.parse.urlencode({"token": token, "password": "browser test password 2026", "code": code}).encode(),
    headers=headers,
)
try:
    response = urllib.request.build_opener(NoRedirect).open(request, timeout=10)
except urllib.error.HTTPError as error:
    assert error.code == 303, f"admin setup failed: {error.code}"
    response = error
cookie = response.headers.get("Set-Cookie", "")
match = re.match(r"__Host-nx-admin=([0-9a-f]{64});", cookie)
assert match, "admin setup did not issue a session"
state = {"cookies": [{
    "name": "__Host-nx-admin", "value": match.group(1), "domain": "127.0.0.1", "path": "/",
    "expires": -1, "httpOnly": True, "secure": True, "sameSite": "Strict",
}], "origins": []}
storage = Path(sys.argv[2])
storage.write_text(json.dumps(state))
storage.chmod(0o600)
totp_test_data = Path(sys.argv[3])
totp_test_data.write_text(json.dumps({"secret": manual_secret, "used_counter": counter}))
totp_test_data.chmod(0o600)
