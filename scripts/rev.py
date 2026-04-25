#!/usr/bin/env python3
"""
Fake-Scanner-Client gegen echten PaperStream NX Manager.
Dient zur Protokoll-Analyse, insbesondere für Duplex/Simplex-Einstellungen.

Verwendung:
  python rev.py http://192.168.178.85:20447 --probe-scansettings
  python rev.py http://192.168.178.85:20447 --dump-job 0
"""
import argparse
import json
import sys
import traceback
from pathlib import Path
from datetime import datetime

import requests


# ---------------------------------------------------------------------------
# Hilfsfunktionen
# ---------------------------------------------------------------------------

def log(msg=""):
    print(msg, flush=True)


def safe_json(r: requests.Response):
    try:
        return r.json()
    except Exception:
        return r.text


def dump(outdir: Path, name: str, method: str, url: str, req_body, r: requests.Response | None, error: str | None = None):
    body = safe_json(r) if r is not None else None
    data = {
        "method": method,
        "url": url,
        "request": req_body,
        "response": {
            "status": r.status_code if r else None,
            "headers": dict(r.headers) if r else None,
            "body": body,
        } if r else None,
        "error": error,
    }
    path = outdir / f"{name}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False, default=str))
    log(f"[dump] {path}")
    return data


def do(session: requests.Session, method: str, url: str, outdir: Path, name: str, **kwargs):
    log(f"\n[{method}] {url}")
    if "params" in kwargs:
        log(f"  params: {kwargs['params']}")
    if "json" in kwargs:
        log(f"  body:   {json.dumps(kwargs['json'], ensure_ascii=False)}")

    try:
        r = session.request(method, url, timeout=15, **kwargs)
    except Exception as e:
        log(f"  ERROR: {e}")
        dump(outdir, name, method, url, kwargs.get("json"), None, str(e))
        return None

    body = safe_json(r)
    log(f"  → HTTP {r.status_code}")
    if isinstance(body, dict):
        log(f"  → {json.dumps(body, ensure_ascii=False)[:300]}")
    else:
        log(f"  → {str(body)[:300]}")

    dump(outdir, name, method, url, kwargs.get("json"), r)
    return r


# ---------------------------------------------------------------------------
# Scanner-Protokoll: Login-Flow
# ---------------------------------------------------------------------------

SCANNER_HEADERS = {
    "User-Agent": "fi-7300NX",
    "Accept": "*/*",
    "Connection": "keep-alive",
}

MAC    = "00:80:17:e7:6f:33"
MODEL  = "fi-7300NX"
SERIAL = "AY9AJ90083"

DEVICE_PAYLOAD_TEMPLATE = {
    "call_timing": "startup",
    "scanner_ip":  None,   # filled at runtime
    "scanner_mac": MAC,
    "scanner_model": MODEL,
    "scanner_name": f"{MODEL}-{SERIAL}",
    "scanner_port": "10447",
    "scanner_protocol": "http",
    "serial_no": SERIAL,
}


def login(base: str, outdir: Path, scanner_ip: str) -> tuple[requests.Session, list[dict]]:
    """Vollständiger Login-Flow. Gibt (session_mit_auth_header, jobs) zurück."""
    s = requests.Session()
    s.headers.update(SCANNER_HEADERS)

    # 1. Heartbeat
    do(s, "GET", f"{base}/NmWebService/heartbeat", outdir, "01-heartbeat")

    # 2. Device-Registrierung
    payload = {**DEVICE_PAYLOAD_TEMPLATE, "scanner_ip": scanner_ip}
    do(s, "POST", f"{base}/NmWebService/device", outdir, "02-device", json=payload)

    # 3. GET /authorization → auth_type herausfinden
    r = do(s, "GET", f"{base}/NmWebService/authorization", outdir, "03-get-auth",
           params={"auth_token": ""})
    auth_type = "none"
    if r and r.status_code == 200:
        auth_type = r.json().get("auth_type", "none")
    log(f"  auth_type = {auth_type!r}")

    # 4. POST /authorization → access_token holen
    r = do(s, "POST", f"{base}/NmWebService/authorization", outdir, "04-post-auth",
           json={"auth_type": auth_type, "scanner_info": {
               "ip": scanner_ip, "mac": MAC, "model": MODEL,
               "name": f"{MODEL}-{SERIAL}", "port": "10447",
               "protocol": "http", "serial_no": SERIAL,
           }})
    access_token = ""
    jobs = []
    if r and r.status_code == 200:
        body = r.json()
        access_token = body.get("access_token", "")
        jobs = body.get("job_info") or body.get("jobs") or []
        log(f"  access_token = {access_token!r}")
        log(f"  jobs ({len(jobs)}): {[j.get('name') for j in jobs]}")
    else:
        log("  [warn] POST /authorization failed — continuing without token")

    if access_token:
        s.headers.update({"Authorization": f"Bearer {access_token}"})

    return s, jobs


# ---------------------------------------------------------------------------
# Scansetting-Analyse
# ---------------------------------------------------------------------------

def fetch_scansetting(s: requests.Session, base: str, outdir: Path, job_id: int, label: str) -> dict | None:
    r = do(s, "GET", f"{base}/NmWebService/scansetting", outdir, f"scansetting-{job_id}-{label}",
           params={"job_id": str(job_id)})
    if r and r.status_code == 200:
        return r.json()
    return None


def extract_source_and_duplex(scansetting: dict) -> dict:
    """Extrahiert alle source/duplex-relevanten Felder aus einer scansetting-Antwort."""
    result = {}
    try:
        sources = scansetting["parameters"]["task"]["actions"]["streams"]["sources"]
        result["source"] = sources.get("source")

        # pixelFormats attributes
        pf_attrs = sources.get("pixelFormats", {}).get("attributes", [])
        for attr in pf_attrs:
            name = attr.get("attribute")
            val  = attr.get("values", {}).get("value")
            result[f"pixelFormats.{name}"] = val

        # feedControls – manchmal steckt duplex hier drin
        fc = sources.get("feedControls", {})
        for section_name, section in fc.items():
            for attr in section.get("attributes", []):
                name = attr.get("attribute")
                val  = attr.get("values", {}).get("value")
                result[f"feedControls.{section_name}.{name}"] = val

        # readControls
        rc = sources.get("readControls", {})
        for section_name, section in rc.items():
            for attr in section.get("attributes", []):
                name = attr.get("attribute")
                val  = attr.get("values", {}).get("value")
                result[f"readControls.{section_name}.{name}"] = val

    except (KeyError, TypeError):
        result["_parse_error"] = True
    return result


def diff_settings(a: dict, b: dict, label_a="vorher", label_b="nachher"):
    """Zeigt alle Felder die sich zwischen zwei scansetting-Snapshots unterscheiden."""
    all_keys = sorted(set(a) | set(b))
    diffs = [(k, a.get(k), b.get(k)) for k in all_keys if a.get(k) != b.get(k)]
    if not diffs:
        log("  → keine Unterschiede gefunden")
    else:
        log(f"  {'Feld':<55} {label_a:<25} {label_b}")
        log("  " + "-" * 100)
        for k, va, vb in diffs:
            log(f"  {k:<55} {str(va):<25} {vb}")


# ---------------------------------------------------------------------------
# Modi
# ---------------------------------------------------------------------------

def cmd_probe_scansettings(base: str, outdir: Path, scanner_ip: str):
    """Holt scansettings für alle Jobs und gibt sie übersichtlich aus."""
    s, jobs = login(base, outdir, scanner_ip)

    if not jobs:
        log("\n[warn] keine Jobs vom Server erhalten, probe job_id 0..4")
        jobs = [{"job_id": i, "name": f"probe-{i}"} for i in range(5)]

    log("\n" + "=" * 60)
    log("SCANSETTINGS ÜBERSICHT")
    log("=" * 60)

    for job in jobs:
        job_id = job.get("job_id") or job.get("id") or 0
        name   = job.get("name", f"job-{job_id}")
        safe   = "".join(c if c.isalnum() or c in "-_" else "_" for c in str(name))

        log(f"\n--- Job {job_id}: {name!r} ---")
        ss = fetch_scansetting(s, base, outdir, job_id, safe)
        if ss:
            fields = extract_source_and_duplex(ss)
            for k, v in sorted(fields.items()):
                log(f"  {k}: {v}")


def cmd_dump_job(base: str, outdir: Path, scanner_ip: str, job_id: int):
    """Holt und dumpt das vollständige scansetting für einen bestimmten Job."""
    s, _ = login(base, outdir, scanner_ip)
    r = do(s, "GET", f"{base}/NmWebService/scansetting", outdir, f"full-scansetting-{job_id}",
           params={"job_id": str(job_id)})
    if r and r.status_code == 200:
        log("\n[full scansetting]")
        log(json.dumps(r.json(), indent=2, ensure_ascii=False))


def cmd_compare(base: str, outdir: Path, scanner_ip: str, job_id: int):
    """
    Interaktiver Vergleich: Scansetting einmal holen, dann auf Eingabe warten
    (du änderst am echten NX Manager), dann nochmal holen und diff ausgeben.
    """
    s, jobs = login(base, outdir, scanner_ip)

    name = f"job-{job_id}"
    for j in jobs:
        if (j.get("job_id") or j.get("id")) == job_id:
            name = j.get("name", name)
            break

    log(f"\n[compare] Job {job_id}: {name!r}")
    log("Hole Snapshot A ...")
    ss_a = fetch_scansetting(s, base, outdir, job_id, "snapshot-A")
    fields_a = extract_source_and_duplex(ss_a) if ss_a else {}
    log("Snapshot A:")
    for k, v in sorted(fields_a.items()):
        log(f"  {k}: {v}")

    log("\n>>> Ändere jetzt die Einstellung am NX Manager und drücke ENTER <<<")
    input()

    log("Hole Snapshot B ...")
    ss_b = fetch_scansetting(s, base, outdir, job_id, "snapshot-B")
    fields_b = extract_source_and_duplex(ss_b) if ss_b else {}

    log("\n[diff A → B]")
    diff_settings(fields_a, fields_b, "vorher", "nachher")

    log("\nSnapshot B (alle Felder):")
    for k, v in sorted(fields_b.items()):
        marker = " ←" if fields_a.get(k) != v else ""
        log(f"  {k}: {v}{marker}")


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="Fake-Scanner-Client für PaperStream NX Manager Protokoll-Analyse"
    )
    parser.add_argument("base_url", help="z.B. http://192.168.178.85:20447")
    parser.add_argument("--scanner-ip", default="192.168.178.250")
    parser.add_argument("--out", default="dumps")

    sub = parser.add_subparsers(dest="cmd")

    sub.add_parser("probe", help="Alle Jobs abfragen und scansettings ausgeben")

    p_dump = sub.add_parser("dump", help="Vollständiges scansetting für einen Job dumpen")
    p_dump.add_argument("job_id", type=int)

    p_cmp = sub.add_parser("compare", help="Diff vor/nach einer Einstellungsänderung")
    p_cmp.add_argument("job_id", type=int)

    args = parser.parse_args()

    ts = datetime.now().strftime("%Y%m%d-%H%M%S")
    outdir = Path(args.out) / ts
    outdir.mkdir(parents=True, exist_ok=True)
    log(f"[out] {outdir.absolute()}")

    base = args.base_url.rstrip("/")

    if args.cmd == "probe" or args.cmd is None:
        cmd_probe_scansettings(base, outdir, args.scanner_ip)
    elif args.cmd == "dump":
        cmd_dump_job(base, outdir, args.scanner_ip, args.job_id)
    elif args.cmd == "compare":
        cmd_compare(base, outdir, args.scanner_ip, args.job_id)
    else:
        parser.print_help()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        log("\n[abort]")
        sys.exit(130)
    except Exception:
        traceback.print_exc()
        sys.exit(1)
