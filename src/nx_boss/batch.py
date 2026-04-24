import datetime
import json
import pathlib
import uuid
from typing import Any

import pydantic

from nx_boss.config import Job


def _now() -> str:
    return datetime.datetime.now().astimezone().isoformat()


class BatchMetadata(pydantic.BaseModel):
    job_name: str
    created_at: str
    completed: bool = False
    files: list[dict[str, Any]] = []


class Batch:
    def __init__(self, *, id: str, dir: pathlib.Path, metadata: BatchMetadata):
        self.id = id
        self._dir = dir
        self._metadata = metadata

    @classmethod
    def create(cls, job: Job) -> Batch:
        batch_id = uuid.uuid4().hex
        batch_dir = pathlib.Path(job.output_path) / batch_id
        batch_dir.mkdir(parents=True)
        metadata = BatchMetadata(
            job_name=job.job_info["name"],
            created_at=_now(),
        )
        b = Batch(id=batch_id, dir=batch_dir, metadata=metadata)
        b._dump_metadata()
        return b

    def add_file(self, *, filename: str, content: bytes, parameters: dict[str, Any]) -> None:
        file_path = self._dir / filename
        # Reject any path component that would escape the batch directory
        if file_path.resolve().parent != self._dir.resolve():
            raise ValueError("bad filename")
        file_path.write_bytes(content)
        self._metadata.files.append(
            {
                "filename": filename,
                "received_at": _now(),
                "parameters": parameters,
            }
        )
        self._dump_metadata()

    def complete(self) -> None:
        self._metadata.completed = True
        self._dump_metadata()

    @property
    def metadata(self) -> BatchMetadata:
        return self._metadata

    def _dump_metadata(self) -> None:
        tmp = self._dir / ".metadata.json"
        final = self._dir / "metadata.json"
        tmp.write_text(self._metadata.model_dump_json())
        tmp.replace(final)


def decode_parameter(raw: bytes) -> dict[str, Any]:
    result: dict[str, Any] = json.loads(raw.decode())
    return result
