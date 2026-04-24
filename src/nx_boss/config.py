import copy
import dataclasses
import pathlib
from typing import Any

import ruamel.yaml

_DEFAULT_JOB_SETTINGS: dict[str, Any] = {
    "continuous_scan": False,
    "show_message": False,
    "message": None,
    "show_thumbnail": False,
    "show_scan_button": False,
    "auto_logout": False,
    "wait_file_transfer": False,
    "show_transfer_completion": False,
    "metadata_setting": None,
    "job_timeout": 0,
}

_DEFAULTS_YAML = pathlib.Path(__file__).parent / "defaults.yaml"


def _load_default_scan_settings() -> dict[str, Any]:
    yaml = ruamel.yaml.YAML()
    result: dict[str, Any] = yaml.load(_DEFAULTS_YAML.read_text())
    return result


def _update_recursive(dest: dict[str, Any], src: dict[str, Any]) -> None:
    for key, value in src.items():
        if isinstance(value, dict):
            _update_recursive(dest[key], value)
            continue
        if key not in dest:
            attrs = [attr for attr in dest.get("attributes", []) if attr["attribute"] == key]
            if attrs:
                _update_recursive(attrs[0]["values"], {"value": value})
                continue
            raise KeyError(f"Unknown attribute {key}")
        default = dest[key]
        if type(value) is type(default):
            dest[key] = value
        elif isinstance(value, bool) and isinstance(default, str):
            # bool must be checked before int — bool is a subclass of int
            dest[key] = "true" if value else "false"
        elif isinstance(value, int) and isinstance(default, str):
            dest[key] = str(value)
        else:
            raise ValueError(
                f"Bad type for attribute {key}: expect {type(default)} but got {type(value)}"
            )


@dataclasses.dataclass(frozen=True)
class Job:
    output_path: str
    job_info: dict[str, Any]
    scan_settings: dict[str, Any]

    @classmethod
    def parse(cls, *, id: int, name: str, job: dict[str, Any]) -> Job:
        job_settings = copy.deepcopy(_DEFAULT_JOB_SETTINGS)
        _update_recursive(job_settings, job.get("job_settings", {}))
        scan_settings = _load_default_scan_settings()
        _update_recursive(
            scan_settings["parameters"]["task"]["actions"]["streams"]["sources"],
            job.get("scan_settings", {}),
        )
        output_path = job["output_path"]
        if not pathlib.Path(output_path).is_dir():
            raise ValueError(f"output_path {output_path!r} is not a directory")
        return cls(
            output_path=output_path,
            job_info={
                "type": 0,
                "job_id": id,
                "name": name,
                "color": job.get("color", "#4D4D4D"),
                "job_setting": job_settings,
                "hierarchy_list": None,
            },
            scan_settings=scan_settings,
        )


@dataclasses.dataclass(frozen=True)
class Config:
    jobs: list[Job]

    @classmethod
    def parse(cls, config_file: dict[str, Any]) -> Config:
        return cls(
            jobs=[
                Job.parse(id=job_id, name=name, job=job)
                for job_id, (name, job) in enumerate(config_file["jobs"].items())
            ],
        )

    @classmethod
    def load(cls, path: pathlib.Path) -> Config:
        yaml = ruamel.yaml.YAML()
        config_file: dict[str, Any] = yaml.load(path.read_text())
        return cls.parse(config_file)
