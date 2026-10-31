#!/usr/bin/env python3
"""Require explicit, existing Caddy storage and configuration bind mounts."""

import json
import subprocess
import sys


def check(compose_files: list[str]) -> None:
    command = ["docker", "compose", "--env-file", "/dev/null"]
    for path in compose_files:
        command += ["-f", path]
    command += ["config", "--format", "json"]
    data = json.loads(subprocess.check_output(command, text=True))
    mounts = data["services"]["caddy"]["volumes"]
    by_target = {mount["target"]: mount for mount in mounts}
    for target in ("/etc/caddy/Caddyfile", "/data", "/config"):
        mount = by_target[target]
        assert mount["type"] == "bind", f"{target} must be an explicit bind mount"
        assert mount.get("bind", {}).get("create_host_path") is False, (
            f"missing {target} source must not be created automatically"
        )


if __name__ == "__main__":
    check(sys.argv[1:] or ["docker-compose.yml"])
    print("Caddy mount sources must exist before Compose startup")
