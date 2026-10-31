#!/usr/bin/env python3
"""Check that every application log line is JSON and request data stays out."""

import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def send(port, method, path, body=None, headers=None):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}", data=body, method=method, headers=headers or {}
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()


def main(binary):
    secrets = ["PRIVATE_JOB_NAME", "PRIVATE_BODY_VALUE", "PRIVATE_QUERY_VALUE",
               "PRIVATE_FILE_NAME", "PRIVATE_API_TOKEN", "deadbeefcafebabedeadbeefcafebabe",
               "PRIVATE_SCANNER_USER", "PRIVATE_SCANNER_PASSWORD", "PRIVATE_SCANNER_BASIC",
               "privateuserlong"]
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        scans = root / "scans"
        scans.mkdir()
        config = root / "config.yaml"
        config.write_text(f"jobs:\n  {secrets[0]}:\n    output_path: {scans}\n")
        pepper = root / "auth-pepper"
        pepper.write_bytes(os.urandom(32))
        pepper.chmod(0o600)
        scanner_port, admin_port = free_port(), free_port()
        assert scanner_port != admin_port
        env = os.environ.copy()
        env["NX_BOSS_AUTH_PEPPER_FILE"] = str(pepper)
        env["NX_BOSS_LOG_LEVEL"] = "debug"
        env["RUST_LOG"] = "nx_boss_rs=debug"
        env["PAPERLESS_API_TOKEN"] = secrets[4]
        env["NX_BOSS_SCANNER_AUTH_PROBE"] = "1"
        log_path = root / "app.log"
        with log_path.open("wb") as log:
            process = subprocess.Popen(
                [binary, "--config", str(config), "--host", "127.0.0.1", "--port", str(scanner_port),
                 "--admin-host", "127.0.0.1", "--admin-port", str(admin_port)],
                env=env, stdout=log, stderr=subprocess.STDOUT,
            )
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        raise AssertionError(f"server exited early: {process.returncode}")
                    try:
                        if send(scanner_port, "GET", "/health")[0] == 200:
                            break
                    except OSError:
                        time.sleep(0.05)
                else:
                    raise AssertionError("server did not become healthy")

                status, headers, _ = send(
                    scanner_port, "GET",
                    f"/NmWebService/scansetting?job_id=0&token={secrets[2]}",
                    headers={"Authorization": "Token PRIVATE_API_TOKEN"},
                )
                assert status == 200
                request_id = headers.get("X-Request-ID")
                assert request_id, "missing request correlation header"
                status, _, _ = send(
                    scanner_port, "POST", "/NmWebService/batch",
                    json.dumps({"job_id": {"password": secrets[1]}}).encode(),
                    {"Content-Type": "application/json"},
                )
                assert status == 422
                assert send(scanner_port, "PUT", f"/NmWebService/batch/{secrets[2]}")[0] == 404
                assert send(
                    scanner_port, "POST", "/NmWebService/authorization",
                    json.dumps({"credentials": {"user_name": secrets[6], "password": secrets[7]},
                                "auth_info_user": secrets[6], "auth_info_pass": secrets[7],
                                "token": secrets[4], "privateuserlong": secrets[9]}).encode(),
                    {"Content-Type": "application/json", "Authorization": f"Basic {secrets[8]}"},
                )[0] == 200
                status, _, data = send(
                    scanner_port, "POST", "/NmWebService/batch",
                    json.dumps({"job_id": 0, "user_name": secrets[6], "password": secrets[7]}).encode(),
                    {"Content-Type": "application/json", "Authorization": f"Basic {secrets[8]}"},
                )
                assert status == 200
                batch_id = json.loads(data)["batch_id"]
                boundary = "logging-test"
                image = (Path(__file__).parent.parent / "tests/fixtures/scan_page.jpg").read_bytes()
                body = (
                    f'--{boundary}\r\nContent-Disposition: form-data; name="image"; '
                    f'filename="{secrets[3]}.jpg"\r\nContent-Type: image/jpeg\r\n\r\n'
                ).encode() + image + (
                    f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="parameter"\r\n\r\n'
                    + json.dumps({"batch_id": batch_id, "user_name": secrets[6], "password": secrets[7]})
                    + f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="imageparameter"\r\n\r\n'
                    + json.dumps({"credentials": {"user_name": secrets[6], "password": secrets[7]}})
                    + f'\r\n--{boundary}--\r\n'
                ).encode()
                assert send(
                    scanner_port, "POST", "/NmWebService/image", body,
                    {"Content-Type": f"multipart/form-data; boundary={boundary}", "Authorization": f"Basic {secrets[8]}"},
                )[0] == 200
                # A token that happens to look like a batch UUID must not be logged
                # before the batch has been looked up in server state.
                invalid_body = body.replace(
                    image, b"not-jpeg"
                ).replace(batch_id.encode(), secrets[5].encode())
                assert send(
                    scanner_port, "POST", "/NmWebService/image", invalid_body,
                    {"Content-Type": f"multipart/form-data; boundary={boundary}", "Authorization": f"Basic {secrets[8]}"},
                )[0] == 400
                assert send(admin_port, "GET", f"/{secrets[2]}")[0] == 200
                assert send(admin_port, "GET", f"/scans/{secrets[2]}")[0] == 200
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                assert process.wait(timeout=10) == 0

        raw = log_path.read_text()
        entries = [json.loads(line) for line in raw.splitlines() if line.strip()]
        assert entries, "no application logs"
        for entry in entries:
            assert {"timestamp", "level", "target", "message"} <= entry.keys(), entry
        for secret in secrets:
            assert secret not in raw, f"sensitive request value was logged: {secret}"
        requests = [entry for entry in entries if entry.get("event") == "http.request"]
        probes = [entry for entry in entries if entry.get("event") == "scanner.auth_probe"]
        assert len(probes) == 1, "missing opt-in scanner auth metadata"
        assert probes[0].get("auth_scheme") == "basic"
        assert probes[0].get("body_format") == "json_object"
        assert "user_name" in probes[0].get("known_fields", "")
        assert "password" in probes[0].get("known_fields", "")
        assert probes[0].get("auth_info_user_type") == "string"
        assert probes[0].get("auth_info_pass_type") == "string"
        scan_probes = [entry for entry in entries if entry.get("event") == "scanner.scan_probe"]
        assert {entry.get("scan_stage") for entry in scan_probes} >= {
            "batch", "image_parameter", "imageparameter"
        }, "missing scan request metadata"
        for entry in scan_probes:
            if entry.get("scan_stage") in {"batch", "image_parameter", "imageparameter"} and "user_name" in entry.get("known_fields", ""):
                assert entry.get("auth_scheme") == "basic", entry.get("scan_stage")
                assert "password" in entry.get("known_fields", "")
        assert any(
            entry.get("request_id") == request_id
            and entry.get("http_route") == "/NmWebService/scansetting"
            and entry.get("http_status") == 200
            and isinstance(entry.get("duration_ms"), int)
            for entry in requests
        ), "missing structured access log or request correlation"

        # The observed device fields authenticate only through the HTTPS proxy.
        experiment_log = root / "experiment.log"
        password_file = root / "scanner-password"
        password_file.write_text("private-test-password\n")
        password_file.chmod(0o600)
        env["NX_BOSS_SCANNER_AUTH_MODE"] = "userpass"
        env["NX_BOSS_SCANNER_USERNAME"] = "private-test-user"
        env["NX_BOSS_SCANNER_PASSWORD_FILE"] = str(password_file)
        with experiment_log.open("wb") as log:
            process = subprocess.Popen(
                [binary, "--config", str(config), "--host", "127.0.0.1", "--port", str(scanner_port),
                 "--admin-host", "127.0.0.1", "--admin-port", str(admin_port)],
                env=env, stdout=log, stderr=subprocess.STDOUT,
            )
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        raise AssertionError(f"experiment server exited early: {process.returncode}")
                    try:
                        if send(scanner_port, "GET", "/health")[0] == 200:
                            break
                    except OSError:
                        time.sleep(0.05)
                else:
                    raise AssertionError("experiment server did not become healthy")
                assert send(scanner_port, "GET", "/NmWebService/authorization")[0] == 403
                assert json.loads(send(scanner_port, "GET", "/NmWebService/authorization",
                                       headers={"X-Forwarded-Proto": "https"})[2])["auth_type"] == "userpass"
                status, _, data = send(
                    scanner_port, "POST", "/NmWebService/authorization",
                    json.dumps({"auth_type": "userpass", "auth_info_user": "private-test-user", "auth_info_pass": "private-test-password"}).encode(),
                    {"Content-Type": "application/json", "Authorization": f"Basic {secrets[8]}"},
                )
                assert status == 403
                assert b"job_info" not in data and b"access_token" not in data
                status, _, data = send(
                    scanner_port, "POST", "/NmWebService/authorization",
                    json.dumps({"auth_type": "userpass", "auth_info_user": "private-test-user", "auth_info_pass": "private-test-password"}).encode(),
                    {"Content-Type": "application/json", "X-Forwarded-Proto": "https"},
                )
                assert status == 200
                token = json.loads(data)["access_token"]
                assert token and token != "unused"
                assert send(scanner_port, "GET", "/NmWebService/scansetting?job_id=0")[0] == 401
                assert send(scanner_port, "GET", "/NmWebService/scansetting?job_id=0",
                            headers={"X-Forwarded-Proto": "https", "Authorization": f"Bearer {token}"})[0] == 200
                assert send(scanner_port, "POST", "/NmWebService/batch",
                            b'{"job_id":0}', {"Content-Type": "application/json"})[0] == 401
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                assert process.wait(timeout=10) == 0
        for secret in secrets:
            assert secret not in experiment_log.read_text(), "experiment logged a sensitive request value"
        experiment_entries = [json.loads(line) for line in experiment_log.read_text().splitlines() if line.strip()]
        experiment_probes = [entry for entry in experiment_entries if entry.get("event") == "scanner.auth_probe"]
        assert len(experiment_probes) == 2
        assert experiment_probes[0].get("auth_info_user_match") is True
        assert experiment_probes[0].get("auth_info_pass_match") is True
        assert "private-test-password" not in experiment_log.read_text()

        # The measured job-group offer is diagnostic only and always denies login.
        job_log = root / "job-probe.log"
        env["NX_BOSS_SCANNER_AUTH_EXPERIMENT"] = "job"
        with job_log.open("wb") as log:
            process = subprocess.Popen(
                [binary, "--config", str(config), "--host", "127.0.0.1", "--port", str(scanner_port),
                 "--admin-host", "127.0.0.1", "--admin-port", str(admin_port)],
                env=env, stdout=log, stderr=subprocess.STDOUT,
            )
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        raise AssertionError(f"job diagnostic exited early: {process.returncode}")
                    try:
                        if send(scanner_port, "GET", "/health")[0] == 200:
                            break
                    except OSError:
                        time.sleep(0.05)
                else:
                    raise AssertionError("job diagnostic did not become healthy")
                assert send(scanner_port, "GET", "/NmWebService/authorization")[0] == 403
                offer = send(scanner_port, "GET", "/NmWebService/authorization",
                             headers={"X-Forwarded-Proto": "https"})
                assert json.loads(offer[2])["auth_type"] == "job"
                status, _, data = send(
                    scanner_port, "POST", "/NmWebService/authorization",
                    json.dumps({"auth_type": "job", "auth_info_job_group_code": "JOBGROUP-SECRET",
                                "auth_timing": 1}).encode(),
                    {"Content-Type": "application/json", "X-Forwarded-Proto": "https"},
                )
                assert status == 401
                assert b"job_info" not in data and b"access_token" not in data
                assert send(scanner_port, "GET", "/NmWebService/scansetting?job_id=0",
                            headers={"X-Forwarded-Proto": "https"})[0] == 401
            finally:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                assert process.wait(timeout=10) == 0
        job_raw = job_log.read_text()
        assert "JOBGROUP-SECRET" not in job_raw
        job_entries = [json.loads(line) for line in job_raw.splitlines() if line.strip()]
        job_probes = [entry for entry in job_entries if entry.get("event") == "scanner.auth_probe"]
        assert len(job_probes) == 1
        assert "auth_info_job_group_code:string" in job_probes[0].get("schema_types", "")
        assert "auth_timing:number" in job_probes[0].get("schema_types", "")
    print(f"{len(entries)} JSON lines; request correlation and secret redaction passed")


if __name__ == "__main__":
    main(sys.argv[1])
