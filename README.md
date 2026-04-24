# nx-boss

Compatible network server for Panasonic PaperStream NX Manager. Allows network scanners (e.g. fi-7300NX) to scan directly over the network — no driver, no Windows, no USB, no proprietary software.

> **Use at your own risk.** Not affiliated with Fujitsu/Ricoh. Internal use only — do not expose to the internet.

## Features

- Emulates the PaperStream NX Manager HTTP API
- Configurable scan jobs via YAML
- Removes the 400 dpi limit imposed by the official software
- Scanned files stored locally with JSON metadata per batch

## Requirements

- Python 3.9+ and [uv](https://docs.astral.sh/uv/) for local development
- Docker for production

## Local development

```bash
# Install dependencies (incl. dev tools)
uv sync --extra dev

# Copy and edit config
cp config.example.yaml config.yaml
mkdir -p out/

# Run the server (binds to localhost only by default)
python -m nx_boss --config config.yaml

# Run tests
python -m pytest tests/ -v

# Lint + type check
ruff check src/ tests/
mypy src/
```

## Docker

```bash
# Prepare config (set output_path to /data)
cp config.example.yaml config.yaml

# Build and start
docker compose up -d

# View logs
docker compose logs -f
```

The server binds to `127.0.0.1:10447` — only reachable from localhost. For LAN access, change the port binding in `docker-compose.yml` to your internal interface IP (e.g. `192.168.1.x:10447:10447`).

## Configuration

```yaml
jobs:
  default:
    output_path: /data         # where scans are saved

  quality:
    output_path: /data
    color: '#ff0000'           # job color in scanner UI
    scan_settings:
      pixelFormats:
        resolution: 600
        jpegQuality: 90

  long_receipt:
    output_path: /data
    scan_settings:
      pixelFormats:
        height: 42304
        width: 5120
```

All available scan settings with their defaults are in [`src/nx_boss/defaults.yaml`](src/nx_boss/defaults.yaml).

## Project structure

```
src/nx_boss/
├── __main__.py    # CLI entry point
├── app.py         # FastAPI app + routes
├── batch.py       # Batch lifecycle + file handling
├── config.py      # Config + Job parsing
└── defaults.yaml  # Default scanner settings

tests/
├── unit/          # Unit tests (batch, config)
└── integration/   # API integration tests
```

## Development workflow

Pre-commit hooks (ruff, mypy, standard checks) run automatically on every commit:

```bash
pre-commit install        # run once after cloning
pre-commit run --all-files  # run manually
```
