# nx-boss-rs

A compatible network server for Panasonic/Fujitsu PaperStream NX Manager, written in Rust.
Allows fi-series network scanners (e.g. fi-7300NX) to scan directly over the network — no driver, no Windows, no USB, no proprietary software required.

> **Use at your own risk.** Not affiliated with Fujitsu/Ricoh/Panasonic.
> Internal LAN use only — do not expose to the internet.

---

## How it works

The fi-7300NX (and similar fi-series scanners) speak a proprietary HTTP JSON protocol — the *NmWebService* API — that normally requires a running PaperStream NX Manager instance on a Windows machine.
nx-boss-rs re-implements that API from scratch in Rust, so the scanner has a server to talk to without any Windows host.

### Scanner session protocol

```
Scanner                         nx-boss-rs
   |                                 |
   |-- GET /heartbeat -------------> |  "are you there?"
   |<- 200 { system_time }           |
   |                                 |
   |-- POST /device ---------------> |  announces MAC, model, serial
   |<- 200                           |
   |                                 |
   |-- GET /authorization ---------> |  negotiates auth type
   |<- 200 { auth_type: "none" }     |
   |                                 |
   |-- POST /authorization --------> |  logs in, receives job list
   |<- 200 { access_token, job_info }|
   |                                 |
   |-- GET /scansetting?job_id=X --> |  fetches scan parameters for chosen job
   |<- 200 { parameters: {...} }     |
   |                                 |
   |-- POST /batch ----------------> |  starts a new scan session
   |<- 200 { batch_id }              |
   |                                 |
   |-- POST /image (multipart) ----> |  uploads one JPEG page
   |   (repeated per page)           |
   |<- 200                           |
   |                                 |
   |-- PUT /batch/{id} ------------> |  signals end of document
   |<- 200                           |
   |                                 |
   |-- DELETE /accesstoken --------> |  logout
   |<- 200                           |
```

The scanner picks a job from the list returned during authorization.
All scan parameters (resolution, color mode, source) come from the job's `scan_settings`, merged on top of `defaults.yaml`.

---

## Features

- Full NmWebService API emulation (heartbeat, device, authorization, scansetting, batch, image)
- Configurable scan jobs via YAML — editable live through the web UI without restart
- No re-encoding: JPEG pages are stored as-is and assembled into PDF using DCTDecode (lossless wrap)
- Removes the 400 dpi limit imposed by the official software (tested: 600 dpi works)
- Optional auto-delivery: when a batch closes, a PDF is written to a `consume_path` (e.g. Paperless-ngx)
- Scan metadata per batch: UUID v6 (time-sortable), `metadata.json` with job name, scanner info, page list
- Retention: automatic archiving to `.tar.zst` and/or deletion after configurable days
- In-memory stale-batch sweep: batches not completed within 24 h are removed
- Web UI: dashboard with live scanner status, scan-job management, scan browser with JPEG thumbnails
- Graceful shutdown on SIGINT/SIGTERM (in-flight requests drain before exit)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                          main.rs                            │
│  CLI parsing (clap) → config load → AppState init → serve  │
│  spawns: retention::run_forever (background task)           │
└──────────────┬──────────────────────────────────────────────┘
               │ axum Router
               ▼
┌─────────────────────────────────────────────────────────────┐
│                       routes/mod.rs                         │
│  force_json middleware  (normalises Content-Type)           │
│  csrf_check middleware  (HX-Request: true for UI mutations) │
│  DefaultBodyLimit       (100 MB, covers large multi-page)   │
│  TraceLayer             (structured request logs)           │
│                                                             │
│  /NmWebService/*  → scanner.rs  batch.rs  image.rs          │
│  /jobs  /scans  /settings  /api/*  →  jobs.rs  scans.rs     │
│                                       settings.rs  ui.rs    │
└──────────────┬──────────────────────────────────────────────┘
               │ shared state (Arc<Mutex<…>>)
               ▼
┌─────────────────────────────────────────────────────────────┐
│                        state.rs                             │
│  scanner:   ScannerState  (online detection, model/serial)  │
│  jobs:      JobStore      (Arc<Mutex<Vec<Job>>>)            │
│  batches:   BatchStore    (Arc<Mutex<HashMap<…, Batch>>>)   │
│  retention: RetentionConfig                                 │
│                                                             │
│  lock!() macro: recovers poisoned Mutex instead of panic    │
└──────────────┬──────────────────────────────────────────────┘
               │
       ┌───────┴────────┐
       ▼                ▼
┌──────────────┐  ┌──────────────────────────────────────────┐
│  config.rs   │  │              batch.rs                    │
│  YAML parse  │  │  Batch::create  – mkdir, metadata.json   │
│  + save      │  │  Batch::add_file – save JPEG, update meta│
│  defaults    │  │  Batch::complete – write PDF, copy to    │
│  merge       │  │                    consume_path          │
└──────────────┘  └──────────────┬───────────────────────────┘
                                 │
                          ┌──────┴──────┐
                          ▼             ▼
                    ┌──────────┐  ┌───────────────┐
                    │  pdf.rs  │  │ retention.rs  │
                    │ JPEG→PDF │  │ sweep stale   │
                    │ DCTDecode│  │ archive/delete│
                    └──────────┘  └───────────────┘
```

### Key design decisions

**No re-encoding.** JPEGs from the scanner are embedded into the PDF using the `DCTDecode` filter — the raw JPEG bytes land directly in the PDF stream. No pixel decode/re-encode cycle, so there is zero quality loss and assembly is fast even at 600 dpi.

**Mutex poison recovery.** The `lock!()` macro recovers a poisoned `Mutex` (caused by a panic in another handler) instead of propagating the panic. A single handler crash cannot take down the whole server.

**Non-blocking I/O discipline.** All filesystem work (`Batch::create`, `add_file`, `complete`, PDF assembly, retention sweeps) runs in `tokio::task::spawn_blocking` so the async executor threads are never stalled on disk. MutexGuards are always scoped to drop before any `await` point to satisfy axum's `Handler: Send` bound.

**CSRF protection.** UI mutation routes (`POST/PUT/DELETE` outside `/NmWebService/*`) require an `HX-Request: true` header. CORS prevents cross-origin requests from setting custom headers, so this blocks CSRF without tokens. Scanner routes are exempt because they are driven by firmware, not a browser.

**Lock-before-IO discipline.** Every mutable operation follows: acquire lock → clone snapshot → release lock → do I/O. Disk writes never happen while a Mutex is held.

**AppState SRP.** `AppState` composes three typed stores (`ScannerState`, `JobStore`, `BatchStore`) instead of bare fields. Each store owns its own `Arc<Mutex<…>>` and exposes a typed API; call sites use the same `lock!()` macro unchanged via `Deref`.

**UUID v6.** Batch IDs are UUID v6 (time-sortable), so scan batches appear in chronological order without sorting.

**Scan settings merge.** The fi-7300NX expects a deeply nested `parameters` object. nx-boss-rs ships a `defaults.yaml` with all known fields and merges the job's `scan_settings` on top, so the YAML config only needs to list overrides.

---

## Project structure

```
src/
├── main.rs            CLI entry point, graceful shutdown, retention task spawn
├── config.rs          Config + Job parsing, YAML serialisation, hex-color validation
├── state.rs           AppState, ScannerState, JobStore, BatchStore, lock!() macro
├── batch.rs           Batch lifecycle: create / add_file / complete, metadata.json
├── pdf.rs             Lossless JPEG→PDF assembly (DCTDecode, no re-encode)
├── retention.rs       Background sweep: stale in-memory batches, archive, delete
└── routes/
    ├── mod.rs         Router, force_json + csrf_check + body-limit + trace middleware
    ├── scanner.rs     NmWebService: heartbeat, device, authorization, scansetting
    ├── batch.rs       POST /NmWebService/batch, PUT /NmWebService/batch/{id}
    ├── image.rs       POST /NmWebService/image (multipart)
    ├── ui.rs          Dashboard, scanner-status API endpoint
    ├── jobs.rs        Web UI: job CRUD (list / new / create / edit / update / delete)
    ├── scans.rs       Web UI: scan list, detail page, file download
    ├── settings.rs    Web UI: retention settings
    └── e2e_test.rs    End-to-end test: full scanner session in-process

tests/
└── fixtures/
    └── scan_page.jpg  Public-domain JPEG fixture (Declaration of Independence excerpt, CC0)

templates/             Askama (Jinja2-like, compiled at build time) HTML templates
scripts/
├── rev.py             Fake-scanner client for protocol analysis (probe/dump/compare)
└── probe_sides.py     Polls source/duplex changes on a real NX Manager
defaults.yaml          Full NmWebService scansetting defaults (merged under job config)
```

---

## Requirements

- Rust stable ≥ 1.80 (uses `let`-chains)
- Docker / Docker Compose (production)

## Local development

```bash
cp config.example.yaml config.yaml
# Edit config.yaml: set output_path to a local directory

cargo run -- --config config.yaml

# Tests (94 total — unit, integration, and full end-to-end scanner session)
cargo test

# Lint
cargo clippy -- -D warnings
```

Web UI: `http://localhost:10447/`

Point the scanner at `http://<your-machine-ip>:10447`.

## Docker (production)

```bash
cp config.example.yaml config.yaml
# Set output_path: /data in config.yaml
# Optionally set consume_path: /paperless/consume

docker compose up -d
docker compose logs -f
```

The server binds to `127.0.0.1:10447` by default — not directly reachable from the LAN.
To allow scanner access, change the port binding in `docker-compose.yml`:

```yaml
ports:
  - "192.168.1.x:10447:10447"   # replace with your server's LAN IP
```

## Configuration

```yaml
jobs:
  # Name shown on the scanner display (max 100 chars)
  Scan to PDF:
    output_path: /data            # where batches are stored
    color: '#2196F3'              # colour shown in the scanner job list

  High Quality:
    output_path: /data
    color: '#E91E63'
    scan_settings:
      pixelFormats:
        resolution: 600
        jpegQuality: 95

  Paperless:
    output_path: /data
    consume_path: /paperless/consume   # PDF delivered here after batch closes
    scan_settings:
      pixelFormats:
        resolution: 300
        pixelFormat: gray8
        jpegQuality: 80
      source: feeder

retention:
  archive_after_days: 30    # compress to .tar.zst after 30 days (0 = off)
  delete_after_days:  90    # delete archive after 90 days (0 = off)
```

All available scan settings with their defaults: [`defaults.yaml`](defaults.yaml).

### Logging

```bash
RUST_LOG=nx_boss_rs=debug cargo run -- --config config.yaml
RUST_LOG=nx_boss_rs=info,tower_http=debug cargo run -- --config config.yaml
```

---

## Protocol analysis tools

`scripts/rev.py` is a fake-scanner client for reverse-engineering or testing against a real PaperStream NX Manager:

```bash
# Show scan settings for all jobs
python scripts/rev.py http://192.168.1.x:20447 probe

# Dump full scansetting JSON for job 0
python scripts/rev.py http://192.168.1.x:20447 dump 0

# Interactive before/after diff (change setting in NX Manager, press Enter)
python scripts/rev.py http://192.168.1.x:20447 compare 0
```

`scripts/probe_sides.py` continuously polls source/duplex changes — useful when mapping scanner UI options to protocol values.

---

## UI language

nx-boss-rs ships with German (`de`) and English (`en`) translations.
Set the language in `config.yaml`:

```yaml
lang: en   # or de (default)
```

The server reads this once at startup. All UI strings, form labels, and validation error messages switch accordingly. No UI toggle exists — it is a per-installation setting, not a per-user preference.

Adding a new language means adding one `pub static XX: Translations = Translations { ... }` block to [`src/translations.rs`](src/translations.rs) and one `"xx" => &XX` arm in `for_lang()`. The struct has 63 fields, all checked at compile time by Rust.

## Scanner authentication

`auth_type: "none"` is intentional. nx-boss-rs is designed for home and small-office use on a **trusted internal LAN** — it must not be exposed to the internet. Within that threat model, requiring a PIN or MAC allowlist adds friction without meaningful security benefit: anyone with physical access to the scanner already has access to the scanned documents.

If your network model requires it, two approaches fit the existing protocol without changing the scanner firmware:

**Option A — MAC allowlist** (simplest):
Add an `allowed_macs` list to `config.yaml`; reject unknown MACs with 403 in the `POST /device` handler.

**Option B — password auth** (protocol-native):
Return `auth_type: "password"` from `GET /authorization`; the scanner prompts the operator for a PIN on its own keypad before showing the job list. Validate in `POST /authorization`.
