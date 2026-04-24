import pathlib

import pytest
from httpx import ASGITransport, AsyncClient

from nx_boss.app import create_app
from nx_boss.config import Config


@pytest.fixture
def output_dir(tmp_path: pathlib.Path) -> pathlib.Path:
    d = tmp_path / "output"
    d.mkdir()
    return d


@pytest.fixture
def config_yaml(output_dir: pathlib.Path) -> str:
    return f"""
jobs:
  default:
    output_path: {output_dir}
  quality:
    output_path: {output_dir}
    scan_settings:
      pixelFormats:
        resolution: 600
"""


@pytest.fixture
def config(config_yaml: str) -> Config:
    import ruamel.yaml

    yaml = ruamel.yaml.YAML()
    return Config.parse(yaml.load(config_yaml))


@pytest.fixture
async def client(config: Config) -> AsyncClient:
    app = create_app(config)
    async with AsyncClient(transport=ASGITransport(app=app), base_url="http://test") as c:
        yield c
