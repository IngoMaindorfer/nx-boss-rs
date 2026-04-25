#!/usr/bin/env python3
"""
Polling-Probe: Erkennt automatisch wenn sich source/Sides im NX Manager ändert.
Einfach starten, dann im NX Manager die Sides-Einstellung durchklicken.
"""
import time, requests, json, sys

BASE   = "http://192.168.178.85:20447"
MAC    = "00:80:17:e7:6f:33"
IP     = "192.168.178.91"
MODEL  = "fi-7300NX"
SERIAL = "AY9AJ90083"
NAME   = f"{MODEL}-{SERIAL}"
JOB_ID = 1

def fresh_scansetting():
    """Kompletter Login + scansetting bei jedem Poll — Token verfällt schnell."""
    s = requests.Session()
    s.headers.update({"User-Agent": MODEL, "Accept": "*/*"})
    s.get(f"{BASE}/NmWebService/heartbeat", timeout=5)
    s.get(f"{BASE}/NmWebService/authorization",
          params={"auth_token": "", "scanner_model": MODEL, "serial_no": SERIAL}, timeout=5)
    r = s.post(f"{BASE}/NmWebService/authorization",
               json={"auth_type": "none", "scanner_info": {
                   "ip": IP, "mac": MAC, "model": MODEL, "name": NAME,
                   "port": "10447", "protocol": "http", "serial_no": SERIAL
               }}, timeout=5)
    token = r.json()["access_token"]
    s.headers["Authorization"] = f"Bearer {token}"
    r = s.get(f"{BASE}/NmWebService/scansetting",
              params={"job_id": str(JOB_ID)}, timeout=5)
    sources = r.json()["parameters"]["task"]["actions"]["streams"]["sources"]
    return sources["source"]

print("Verbinde...")
prev = fresh_scansetting()
print(f"Aktuell: source = {prev!r}")
print("\n>>> Wechsle jetzt die 'Sides' Einstellung im NX Manager <<<")
print("    Klicke alle Optionen durch — Änderungen werden sofort angezeigt.\n")

seen = {prev: "Start"}

while True:
    time.sleep(2)
    try:
        curr = fresh_scansetting()
        if curr != prev:
            print(f"\n  {prev!r}  →  {curr!r}")
            seen[curr] = "?"
            prev = curr
        else:
            sys.stdout.write(".")
            sys.stdout.flush()
    except KeyboardInterrupt:
        break
    except Exception as e:
        sys.stdout.write(f"[{e}]")
        sys.stdout.flush()

print(f"\n\nGesammelte source-Werte:")
for src in seen:
    print(f"  {src!r}")
