# nx-boss-rs

Compatible network server for Panasonic PaperStream NX Manager, written in Rust. Allows network scanners (e.g. fi-7300NX) to scan directly over the network — no driver, no Windows, no USB, no proprietary software.

> **Use at your own risk.** Not affiliated with Fujitsu/Ricoh. Internal use only — do not expose to the internet.

## Features

- Emulates the PaperStream NX Manager HTTP API
- Configurable scan jobs via YAML (live CRUD through the web UI)
- Removes the 400 dpi limit imposed by the official software
- Scanned files stored locally with JSON metadata per batch (UUID v6, time-sortable)
- Optional PDF delivery to a Paperless-ngx consume folder
- Web UI: dashboard, scan-job management, scan browser with JPEG thumbnails
- Graceful shutdown on SIGINT/SIGTERM

## Requirements

- Rust stable (for building from source)
- Docker / Docker Compose (for production)

## Local development

```bash
# Copy and edit config
cp config.example.yaml config.yaml

# Run the server (binds to 0.0.0.0:10447 by default)
cargo run -- --config config.yaml

# Run tests
cargo test

# Lint
cargo clippy -- -D warnings
```

The web UI is available at `http://localhost:10447/`.

## Docker

```bash
# Prepare config (set output_path to /data)
cp config.example.yaml config.yaml

# Build and start
docker compose up -d

# View logs
docker compose logs -f
```

The server binds to `127.0.0.1:10447` in Docker — only reachable from localhost. For LAN access, change the port binding in `docker-compose.yml` to your internal interface IP (e.g. `192.168.1.x:10447:10447`).

## Configuration

```yaml
jobs:
  default:
    output_path: /data          # where scans are saved

  quality:
    output_path: /data
    color: '#ff0000'            # job color in scanner UI
    scan_settings:
      pixelFormats:
        resolution: 600
        jpegQuality: 90

  paperless:
    output_path: /data
    consume_path: /paperless/consume   # auto-deliver PDF here when batch closes
    scan_settings:
      pixelFormats:
        resolution: 300
        pixelFormat: gray8
```

All available scan settings with their defaults are in [`defaults.yaml`](defaults.yaml).

## Project structure

```
src/
├── main.rs         # CLI entry point + graceful shutdown
├── config.rs       # Config + Job parsing / persistence
├── state.rs        # Shared AppState (jobs, batches, scanner info)
├── batch.rs        # Batch lifecycle, metadata, PDF delivery
├── pdf.rs          # JPEG → PDF assembly (DCTDecode, no re-encode)
└── routes/
    ├── mod.rs      # Router, force_json middleware, body-size limit
    ├── ui.rs       # Web UI handlers + templates
    ├── scanner.rs  # Scanner protocol (heartbeat, device, auth, settings)
    ├── batch.rs    # POST/PUT /NmWebService/batch
    └── image.rs    # POST /NmWebService/image

templates/          # Askama (Jinja2-like) HTML templates
```
