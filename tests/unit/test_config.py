import pathlib

import pytest

from nx_boss.config import Config, Job, _update_recursive


def test_update_recursive_simple(tmp_path: pathlib.Path) -> None:
    dest = {"a": "old", "b": 1}
    _update_recursive(dest, {"a": "new"})
    assert dest["a"] == "new"
    assert dest["b"] == 1


def test_update_recursive_int_to_str() -> None:
    dest = {"resolution": "300"}
    _update_recursive(dest, {"resolution": 600})
    assert dest["resolution"] == "600"


def test_update_recursive_bool_to_str() -> None:
    dest = {"enabled": "false"}
    _update_recursive(dest, {"enabled": True})
    assert dest["enabled"] == "true"


def test_update_recursive_nested() -> None:
    dest = {"outer": {"inner": "old"}}
    _update_recursive(dest, {"outer": {"inner": "new"}})
    assert dest["outer"]["inner"] == "new"


def test_update_recursive_attribute_lookup() -> None:
    dest = {"attributes": [{"attribute": "resolution", "values": {"value": "300"}}]}
    _update_recursive(dest, {"resolution": "600"})
    assert dest["attributes"][0]["values"]["value"] == "600"


def test_update_recursive_unknown_key_raises() -> None:
    dest = {"known": "x"}
    with pytest.raises(KeyError, match="Unknown attribute"):
        _update_recursive(dest, {"unknown": "y"})


def test_update_recursive_type_mismatch_raises() -> None:
    dest = {"val": [1, 2, 3]}
    with pytest.raises(ValueError, match="Bad type"):
        _update_recursive(dest, {"val": "string"})


def test_config_parse_basic(tmp_path: pathlib.Path) -> None:
    import ruamel.yaml

    yaml = ruamel.yaml.YAML()
    config_file = yaml.load(f"""
jobs:
  default:
    output_path: {tmp_path}
""")
    config = Config.parse(config_file)
    assert len(config.jobs) == 1
    assert config.jobs[0].job_info["name"] == "default"
    assert config.jobs[0].job_info["job_id"] == 0


def test_config_parse_multiple_jobs(tmp_path: pathlib.Path) -> None:
    import ruamel.yaml

    yaml = ruamel.yaml.YAML()
    config_file = yaml.load(f"""
jobs:
  first:
    output_path: {tmp_path}
  second:
    output_path: {tmp_path}
""")
    config = Config.parse(config_file)
    assert len(config.jobs) == 2
    assert config.jobs[1].job_info["job_id"] == 1


def test_job_parse_with_scan_settings(tmp_path: pathlib.Path) -> None:
    job = Job.parse(
        id=0,
        name="quality",
        job={
            "output_path": str(tmp_path),
            "scan_settings": {"pixelFormats": {"resolution": 600}},
        },
    )
    attrs = job.scan_settings["parameters"]["task"]["actions"]["streams"]["sources"][
        "pixelFormats"
    ]["attributes"]
    resolution = next(a["values"]["value"] for a in attrs if a["attribute"] == "resolution")
    assert resolution == "600"


def test_job_parse_invalid_output_path() -> None:
    with pytest.raises(ValueError, match="output_path"):
        Job.parse(id=0, name="x", job={"output_path": "/nonexistent/path/xyz"})


def test_config_load_from_file(tmp_path: pathlib.Path) -> None:
    config_path = tmp_path / "config.yaml"
    config_path.write_text(f"jobs:\n  default:\n    output_path: {tmp_path}\n")
    config = Config.load(config_path)
    assert config.jobs[0].job_info["name"] == "default"
