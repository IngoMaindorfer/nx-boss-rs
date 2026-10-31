#!/usr/bin/env python3
"""Keep the published OpenAPI contract aligned with the scanner router."""

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = json.loads((ROOT / "docs/openapi.json").read_text())
SOURCE = (ROOT / "src/routes/mod.rs").read_text()
SCANNER_ROUTER = SOURCE.split("fn scanner_routes()", 1)[1].split("#[cfg(test)]", 1)[0]


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def main():
    declared = {
        (path, method)
        for path, method in re.findall(
            r'\.route\(\s*"([^"]+)"\s*,\s*(get|post|put|delete)\(',
            SCANNER_ROUTER,
        )
    }
    documented = {
        (path, method)
        for path, methods in SPEC["paths"].items()
        for method in methods
        if method in {"get", "post", "put", "delete", "patch"}
    }
    check(declared == documented, f"scanner route drift: missing={declared-documented}, extra={documented-declared}")

    operation_ids = []
    for path, method in documented:
        operation = SPEC["paths"][path][method]
        operation_ids.append(operation.get("operationId"))
        check(operation.get("responses"), f"missing responses: {method} {path}")
        check("security" in operation, f"unspecified security: {method} {path}")
        if "{" in path:
            names = set(re.findall(r"\{([^}]+)\}", path))
            parameters = {p["name"] for p in operation.get("parameters", []) if p.get("in") == "path" and p.get("required")}
            check(names == parameters, f"path parameters drift: {method} {path}")
    check(len(operation_ids) == len(set(operation_ids)), "duplicate operationId")

    references = re.findall(r'"\$ref"\s*:\s*"([^"]+)"', json.dumps(SPEC))
    for reference in references:
        check(reference.startswith("#/"), f"external reference: {reference}")
        target = SPEC
        for part in reference[2:].split("/"):
            part = part.replace("~1", "/").replace("~0", "~")
            check(part in target, f"broken reference: {reference}")
            target = target[part]
    print(f"OpenAPI covers {len(documented)} scanner operations; references and security fields valid")


if __name__ == "__main__":
    main()
