#!/usr/bin/env python3
"""Exercise SIGTERM while an image upload is still being received."""

import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error
from pathlib import Path


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def request(port, method, path, body=None, token=None):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=body,
        method=method,
        headers={"Content-Type": "application/json", "X-Forwarded-Proto": "https", **({"Authorization": "Bearer " + token} if token else {})},
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as response:
        return response.code, response.read()


def start_server(binary, config, scanner_port, admin_port, log, mode):
    env = os.environ.copy()
    env["NX_BOSS_SCANNER_AUTH_MODE"] = mode
    env["NX_BOSS_SCANNER_USERNAME"] = "shutdown-scanner"
    env["NX_BOSS_SCANNER_PASSWORD_FILE"] = str(config.parent / "scanner-password")
    env["NX_BOSS_AUTH_PEPPER_FILE"] = str(config.parent / "auth-pepper")
    process = subprocess.Popen(
        [binary, "--config", str(config), "--host", "127.0.0.1", "--port", str(scanner_port),
         "--admin-host", "127.0.0.1", "--admin-port", str(admin_port)],
        stdout=log,
        stderr=subprocess.STDOUT,
        env=env,
    )
    for _ in range(100):
        if process.poll() is not None:
            raise AssertionError(f"server exited early: {process.returncode}")
        try:
            if request(scanner_port, "GET", "/health")[0] == 200:
                return process
        except OSError:
            time.sleep(0.05)
    raise AssertionError("server did not become healthy")


def stop_server(process):
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
    assert process.wait(timeout=10) == 0, "server did not finish SIGTERM cleanly"


def login(port):
    status, data = request(port, "POST", "/NmWebService/authorization", json.dumps({
        "auth_type": "userpass", "auth_info_user": "shutdown-scanner", "auth_info_pass": "shutdown-test-password",
    }).encode())
    assert status == 200, "disposable scanner login failed"
    return json.loads(data)["access_token"]


def run(binary, mode):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        scans = root / "scans"
        scans.mkdir()
        config = root / "config.yaml"
        config.write_text(f"jobs:\n  ShutdownTest:\n    output_path: {scans}\n")
        password = root / "scanner-password"
        password.write_text("shutdown-test-password")
        password.chmod(0o600)
        pepper = root / "auth-pepper"
        pepper.write_bytes(os.urandom(32))
        pepper.chmod(0o600)
        scanner_port, admin_port = free_port(), free_port()
        assert scanner_port != admin_port
        with (root / "server.log").open("wb") as log:
            process = start_server(binary, config, scanner_port, admin_port, log, mode)
            try:
                token = login(scanner_port) if mode == "userpass" else None
                status, data = request(scanner_port, "POST", "/NmWebService/batch", b'{"job_id":0}', token)
                assert status == 200
                batch_id = json.loads(data)["batch_id"]
                boundary = "shutdown-test-boundary"
                image = (Path(__file__).parent.parent / "tests/fixtures/scan_page.jpg").read_bytes()
                body = (
                    f'--{boundary}\r\nContent-Disposition: form-data; name="image"; filename="page.jpg"\r\n'
                    'Content-Type: image/jpeg\r\n\r\n'
                ).encode() + image + (
                    f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="parameter"\r\n\r\n'
                    + json.dumps({"batch_id": batch_id})
                    + f'\r\n--{boundary}--\r\n'
                ).encode()
                with socket.create_connection(("127.0.0.1", scanner_port), timeout=5) as upload:
                    upload.settimeout(5)
                    upload.sendall((
                        "POST /NmWebService/image HTTP/1.1\r\n"
                        "Host: 127.0.0.1\r\n"
                        f"Content-Type: multipart/form-data; boundary={boundary}\r\n"
                        f"Content-Length: {len(body)}\r\n"
                        + (f"Authorization: Bearer {token}\r\n" if token else "")
                        + "X-Forwarded-Proto: https\r\nConnection: close\r\n\r\n"
                    ).encode())
                    upload.sendall(body[: len(body) // 2])
                    time.sleep(0.1)
                    process.send_signal(signal.SIGTERM)
                    time.sleep(0.1)
                    assert process.poll() is None, "server stopped before the upload finished"
                    upload.sendall(body[len(body) // 2 :])
                    response = bytearray()
                    while chunk := upload.recv(4096):
                        response.extend(chunk)
                    assert response.startswith(b"HTTP/1.1 200 "), response[:200]
                assert process.wait(timeout=10) == 0, "server did not finish SIGTERM cleanly"
                metadata_path = scans / batch_id / "metadata.json"
                metadata = json.loads(metadata_path.read_text())
                assert not metadata["completed"]
                assert [item["filename"] for item in metadata["files"]] == ["page.jpg"]
                assert (scans / batch_id / "page.jpg").read_bytes() == image

                restarted = start_server(binary, config, scanner_port, admin_port, log, mode)
                try:
                    if mode == "userpass":
                        assert request(scanner_port, "PUT", f"/NmWebService/batch/{batch_id}", token=token)[0] == 401
                        token = login(scanner_port)
                    assert request(scanner_port, "PUT", f"/NmWebService/batch/{batch_id}", token=token)[0] == 200
                finally:
                    stop_server(restarted)
                assert json.loads(metadata_path.read_text())["completed"]
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
    print(f"{mode}: SIGTERM drained upload; batch recovered and completed after restart")


if __name__ == "__main__":
    for selected_mode in ["none", "userpass"]:
        run(sys.argv[1], selected_mode)
