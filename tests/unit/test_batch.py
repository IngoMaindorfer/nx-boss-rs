import pathlib

import pytest

from nx_boss.batch import Batch, BatchMetadata


def _make_batch(tmp_path: pathlib.Path) -> Batch:
    d = tmp_path / "batch"
    d.mkdir()
    metadata = BatchMetadata(job_name="test", created_at="2024-01-01T00:00:00+00:00")
    b = Batch(id="abc123", dir=d, metadata=metadata)
    return b


def test_add_file_valid(tmp_path: pathlib.Path) -> None:
    batch = _make_batch(tmp_path)
    batch.add_file(filename="scan.jpg", content=b"fakeimage", parameters={})
    assert (tmp_path / "batch" / "scan.jpg").read_bytes() == b"fakeimage"
    assert batch.metadata.files[0]["filename"] == "scan.jpg"


def test_add_file_rejects_path_traversal(tmp_path: pathlib.Path) -> None:
    batch = _make_batch(tmp_path)
    with pytest.raises(ValueError, match="bad filename"):
        batch.add_file(filename="../escape.txt", content=b"oops", parameters={})


def test_add_file_rejects_absolute_path(tmp_path: pathlib.Path) -> None:
    batch = _make_batch(tmp_path)
    with pytest.raises((ValueError, OSError)):
        batch.add_file(filename="/etc/passwd", content=b"oops", parameters={})


def test_add_file_metadata_persisted(tmp_path: pathlib.Path) -> None:
    batch = _make_batch(tmp_path)
    batch.add_file(filename="page1.jpg", content=b"data", parameters={"key": "val"})
    meta_path = tmp_path / "batch" / "metadata.json"
    assert meta_path.exists()
    import json

    meta = json.loads(meta_path.read_text())
    assert meta["files"][0]["filename"] == "page1.jpg"
    assert meta["files"][0]["parameters"] == {"key": "val"}


def test_complete_sets_flag(tmp_path: pathlib.Path) -> None:
    batch = _make_batch(tmp_path)
    assert not batch.metadata.completed
    batch.complete()
    assert batch.metadata.completed


def test_complete_persists_metadata(tmp_path: pathlib.Path) -> None:
    batch = _make_batch(tmp_path)
    batch.complete()
    import json

    meta = json.loads((tmp_path / "batch" / "metadata.json").read_text())
    assert meta["completed"] is True


def test_no_tmp_metadata_orphan_after_write(tmp_path: pathlib.Path) -> None:
    batch = _make_batch(tmp_path)
    batch.add_file(filename="x.jpg", content=b"x", parameters={})
    assert not (tmp_path / "batch" / ".metadata.json").exists()
