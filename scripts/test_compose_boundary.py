#!/usr/bin/env python3
"""Check that Caddy alone publishes ports and shares port settings with nx-boss."""

import json
import os
import subprocess
import sys
from pathlib import Path


def compose_config(compose_files: list[str], ports: dict[str, int]) -> dict:
    command = ["docker", "compose"]
    for path in compose_files:
        command += ["-f", path]
    command += ["config", "--format", "json"]
    env = os.environ.copy()
    env.update({name: str(port) for name, port in ports.items()})
    return json.loads(subprocess.check_output(command, text=True, env=env))


def check(compose_files: list[str], ports: dict[str, int]) -> None:
    config = compose_config(compose_files, ports)
    services = config["services"]
    assert config["secrets"]["cloudflare_api_token"]["file"].endswith(
        "/secrets/cloudflare_api_token"
    ), "Cloudflare token belongs in secrets/"
    app = services["nx-boss-rs"]
    caddy = services["caddy"]

    assert app["build"]["target"] == "app"
    assert caddy["build"]["target"] == "caddy"
    assert caddy["build"]["dockerfile"] == "Dockerfile"
    assert any(
        mount["target"] == "/etc/caddy/Caddyfile"
        and mount["source"].endswith("/deploy/Caddyfile")
        and mount["read_only"]
        for mount in caddy["volumes"]
    ), "Caddy must mount the deployment config from deploy/"
    assert {secret["source"] for secret in caddy["secrets"]} == {"cloudflare_api_token"}
    assert any(mount["target"] == "/data" and mount["source"].endswith("caddy-acme/data") for mount in caddy["volumes"])

    assert not app.get("ports"), "nx-boss must not publish host ports"
    assert not app.get("expose"), "nx-boss does not need Docker expose metadata"
    assert set(services) == {"nx-boss-rs", "caddy"}, "unexpected Compose service"

    published = {(int(item["published"]), int(item["target"])) for item in caddy["ports"]}
    expected = {
        (ports["NX_BOSS_SCANNER_PORT"], ports["NX_BOSS_SCANNER_PORT"]),
        (ports["NX_BOSS_SCANNER_HTTPS_PORT"], ports["NX_BOSS_SCANNER_HTTPS_PORT"]),
        (ports["NX_BOSS_ADMIN_HTTPS_PORT"], ports["NX_BOSS_ADMIN_HTTPS_PORT"]),
    }
    assert published == expected, f"Caddy port mappings differ: {published} != {expected}"

    caddy_env = caddy["environment"]
    for setting, port in (
        ("NX_BOSS_SCANNER_HTTP_PORT", ports["NX_BOSS_SCANNER_PORT"]),
        ("NX_BOSS_SCANNER_HTTPS_PORT", ports["NX_BOSS_SCANNER_HTTPS_PORT"]),
        ("NX_BOSS_ADMIN_HTTPS_PORT", ports["NX_BOSS_ADMIN_HTTPS_PORT"]),
        ("NX_BOSS_UPSTREAM_SCANNER_PORT", ports["NX_BOSS_INTERNAL_SCANNER_PORT"]),
        ("NX_BOSS_UPSTREAM_ADMIN_PORT", ports["NX_BOSS_INTERNAL_ADMIN_PORT"]),
    ):
        assert caddy_env[setting] == str(port), f"Caddy {setting} differs from Compose"

    command = app["command"]
    assert command[command.index("--port") + 1] == str(ports["NX_BOSS_INTERNAL_SCANNER_PORT"])
    assert command[command.index("--admin-port") + 1] == str(
        ports["NX_BOSS_INTERNAL_ADMIN_PORT"]
    )
    healthcheck = app["healthcheck"]["test"]
    assert healthcheck[-2:] == ["--port", str(ports["NX_BOSS_INTERNAL_SCANNER_PORT"])]
    edge_health = caddy["healthcheck"]["test"]
    assert edge_health[0] == "CMD-SHELL"
    assert "127.0.0.1" in edge_health[1] and "/health" in edge_health[1]
    assert str(ports["NX_BOSS_SCANNER_PORT"]) in edge_health[1]


if __name__ == "__main__":
    assert not Path("docker-compose.dns.yml").exists(), "use one Compose file"
    assert not Path("Dockerfile.caddy").exists(), "build both images from one Dockerfile"
    assert not Path("Caddyfile.dns").exists(), "use one Caddyfile"
    assert not Path("Caddyfile").exists(), "keep Caddy configuration in deploy/"
    assert not Path("config.example.yaml").exists(), "keep examples in examples/"
    assert not Path("defaults.yaml").exists(), "keep embedded resources in resources/"
    assert Path("examples/config.yaml").is_file()
    assert Path("resources/defaults.yaml").is_file()
    assert "COPY resources/defaults.yaml ./resources/defaults.yaml" in Path("Dockerfile").read_text()
    caddyfile = Path("deploy/Caddyfile").read_text()
    assert "dns cloudflare" in caddyfile and "tls internal" not in caddyfile
    assert not any(
        line.lstrip().upper().startswith("EXPOSE ")
        for line in Path("Dockerfile").read_text().splitlines()
    ), "nx-boss image must not advertise internal ports as exposed"
    files = sys.argv[1:] or ["docker-compose.yml"]
    for values in (
        {
            "NX_BOSS_SCANNER_PORT": 20447,
            "NX_BOSS_SCANNER_HTTPS_PORT": 20448,
            "NX_BOSS_ADMIN_HTTPS_PORT": 443,
            "NX_BOSS_INTERNAL_SCANNER_PORT": 10447,
            "NX_BOSS_INTERNAL_ADMIN_PORT": 10448,
        },
        {
            "NX_BOSS_SCANNER_PORT": 21447,
            "NX_BOSS_SCANNER_HTTPS_PORT": 21448,
            "NX_BOSS_ADMIN_HTTPS_PORT": 8443,
            "NX_BOSS_INTERNAL_SCANNER_PORT": 11447,
            "NX_BOSS_INTERNAL_ADMIN_PORT": 11448,
        },
    ):
        check(files, values)
    print("Only Caddy publishes the configured ports")
