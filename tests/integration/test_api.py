import json
import pathlib

from httpx import AsyncClient


async def test_heartbeat(client: AsyncClient) -> None:
    resp = await client.get("/NmWebService/heartbeat")
    assert resp.status_code == 200
    assert "system_time" in resp.json()


async def test_device_registration(client: AsyncClient) -> None:
    resp = await client.post(
        "/NmWebService/device",
        json={
            "call_timing": "startup",
            "scanner_ip": "192.168.1.10",
            "scanner_mac": "AA:BB:CC:DD:EE:FF",
            "scanner_model": "fi-7300NX",
            "scanner_name": "Scanner1",
            "scanner_port": "10447",
            "scanner_protocol": "http",
            "serial_no": "SN12345",
        },
    )
    assert resp.status_code == 200
    assert resp.json()["server_version"] == "2.6.0.4"


async def test_get_authorization(client: AsyncClient) -> None:
    resp = await client.get("/NmWebService/authorization", params={"auth_token": "token"})
    assert resp.status_code == 200
    assert resp.json()["auth_type"] == "none"


async def test_post_authorization_returns_jobs(client: AsyncClient) -> None:
    resp = await client.post("/NmWebService/authorization")
    assert resp.status_code == 200
    data = resp.json()
    assert data["job_group_name"] == "nx-boss"
    assert len(data["job_info"]) == 2
    assert data["job_info"][0]["name"] == "default"


async def test_get_scansetting(client: AsyncClient) -> None:
    resp = await client.get("/NmWebService/scansetting", params={"job_id": "0"})
    assert resp.status_code == 200
    data = resp.json()
    assert "parameters" in data


async def test_create_batch(client: AsyncClient) -> None:
    resp = await client.post("/NmWebService/batch", json={"job_id": "0"})
    assert resp.status_code == 200
    assert "batch_id" in resp.json()


async def test_delete_accesstoken(client: AsyncClient) -> None:
    resp = await client.delete("/NmWebService/accesstoken")
    assert resp.status_code == 200
    assert resp.json()["MediaType"] == "application/json"


async def test_full_scan_flow(client: AsyncClient, output_dir: pathlib.Path) -> None:
    # 1. create batch
    resp = await client.post("/NmWebService/batch", json={"job_id": "0"})
    batch_id = resp.json()["batch_id"]

    # 2. upload image
    param_bytes = json.dumps({"batch_id": batch_id, "page": 1}).encode()
    resp = await client.post(
        "/NmWebService/image",
        files={
            "image": ("scan.jpg", b"fakejpegdata", "image/jpeg"),
            "imageparameter": ("imageparameter.json", b"{}", "application/json"),
            "parameter": ("parameter", param_bytes, "application/octet-stream"),
        },
    )
    assert resp.status_code == 200

    # 3. verify file was written
    batch_dirs = [d for d in output_dir.iterdir() if d.is_dir()]
    assert len(batch_dirs) == 1
    assert (batch_dirs[0] / "scan.jpg").read_bytes() == b"fakejpegdata"

    # 4. complete batch
    resp = await client.put(
        f"/NmWebService/batch/{batch_id}",
        files={"parameter": ("p", b"{}", "application/octet-stream")},
    )
    assert resp.status_code == 200

    # 5. verify metadata
    meta_path = batch_dirs[0] / "metadata.json"
    assert meta_path.exists()
    meta = json.loads(meta_path.read_text())
    assert meta["completed"] is True
    assert meta["files"][0]["filename"] == "scan.jpg"


async def test_image_upload_rejects_path_traversal(
    client: AsyncClient, output_dir: pathlib.Path
) -> None:
    resp = await client.post("/NmWebService/batch", json={"job_id": "0"})
    batch_id = resp.json()["batch_id"]

    param_bytes = json.dumps({"batch_id": batch_id}).encode()
    resp = await client.post(
        "/NmWebService/image",
        files={
            "image": ("../escape.txt", b"evil", "application/octet-stream"),
            "imageparameter": ("imageparameter.json", b"{}", "application/json"),
            "parameter": ("parameter", param_bytes, "application/octet-stream"),
        },
    )
    assert resp.status_code in (400, 422, 500)
    assert not (output_dir / "escape.txt").exists()
