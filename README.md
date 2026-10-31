# nx-boss-rs

Rust server for the PaperStream NX Manager scanner protocol. It accepts JPEG pages from a compatible network scanner, stores batches, assembles PDFs, and can send completed PDFs to Paperless-ngx through its HTTP API.

This is an independent protocol implementation. Compatibility with a particular scanner and firmware must be verified on the device. Scanner mode `none` accepts unauthenticated scans; `userpass` verifies the fi-7300NX's JSON login over HTTPS and issues a bearer for scan requests. Keep all published ports inside the trusted LAN and see [the deployment and protocol plan](docs/security-review.md).

## Run locally

```bash
mkdir -p config
cp examples/config.yaml config/local.yaml
# Set each output_path to a writable directory.
cargo run -- --config config/local.yaml --host 127.0.0.1
```

Before starting, create a private 32-byte pepper file and set `NX_BOSS_AUTH_PEPPER_FILE` to its absolute path. Keep this file outside the repository and back it up separately from `config.yaml`. The app fails closed if it is missing or has permissive file permissions. On first start, nx-boss writes a private `admin-setup-token` next to the config. Visit the admin listener through HTTPS, enter that one-time token, and register an Admin passkey. The token is deleted after setup. Add a second passkey under `/account` for recovery. Production Admin access is only through Caddy HTTPS. Set `NX_BOSS_ADMIN_DOMAIN` and `NX_BOSS_ADMIN_HTTPS_PORT` to the exact public Admin hostname and port; changing the hostname requires new passkeys.

The scanner API listens on port `10447`; the admin UI listens on `http://127.0.0.1:10448/`. Both bind to loopback by default. To connect the physical scanner, bind its listener to the host's LAN address using `--host`. The admin listener accepts its bind address as a Host header; use `--admin-allowed-host` for any additional hostname. Admin pages require an enrolled passkey and a private session cookie. Sensitive scanner and Paperless settings require a fresh passkey confirmation.

```bash
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
npm ci
npx playwright install chromium
npm run test:e2e
```

The admin UI supports German and English per browser. Use the language selector in the sidebar; the choice is stored in a non-secret cookie. The default for new browsers is editable under Settings. Dates use the browser's local time zone and selected language. The appearance selector follows the operating system by default, or saves a light/dark override in browser storage. Chromium tests exercise the UI through an isolated HTTPS proxy and test account. CI also checks that both translation sets have nonempty values and matching format placeholders.

## Docker Compose

```bash
mkdir -p config scans secrets caddy-acme/data caddy-acme/config
cp examples/config.yaml config/config.yaml
# Edit config/config.yaml: use output_path: /data and set paperless.url if needed.
# Put PAPERLESS_API_TOKEN, NX_BOSS_SERVER_IP, NX_BOSS_ADMIN_IP and
# NX_BOSS_ADMIN_DOMAIN=nx.maindorfer.name in a private .env.
# Put a scoped Cloudflare DNS API token in ./secrets/cloudflare_api_token (one line).
python3 -c 'import os,pathlib; p=pathlib.Path("secrets/auth_pepper"); p.write_bytes(os.urandom(32)); p.chmod(0o600)'
printf 'NX_BOSS_UID=%s\nNX_BOSS_GID=%s\n' "$(id -u)" "$(id -g)" >> .env
chmod 700 config scans secrets caddy-acme caddy-acme/data caddy-acme/config
chmod 600 .env secrets/cloudflare_api_token secrets/auth_pepper config/config.yaml
docker compose up -d --build
```

There is **one `docker-compose.yml`** with two services: nx-boss and Caddy. The single `Dockerfile` builds both images through separate targets; Caddy needs its Cloudflare DNS module because the LAN-only host name uses DNS-01 for certificate issuance and renewal. `deploy/Caddyfile` defines the three routes. The nx-boss container has neither published ports nor `expose` metadata. Caddy forwards scanner HTTP and HTTPS to the app's internal HTTP listener and admin HTTPS to its separate internal HTTP listener. All five port values have one source in Compose and can be overridden in `.env`:

| Variable | Default | Purpose |
| --- | ---: | --- |
| `NX_BOSS_SCANNER_PORT` | `20447` | Public scanner HTTP port on `NX_BOSS_SERVER_IP` |
| `NX_BOSS_SCANNER_HTTPS_PORT` | `20448` | Public scanner HTTPS port on `NX_BOSS_SERVER_IP` |
| `NX_BOSS_ADMIN_HTTPS_PORT` | `443` | Public admin HTTPS port on `NX_BOSS_ADMIN_IP` |
| `NX_BOSS_INTERNAL_SCANNER_PORT` | `10447` | Scanner HTTP port within the Compose network |
| `NX_BOSS_INTERNAL_ADMIN_PORT` | `10448` | Admin HTTP port within the Compose network |

`NX_BOSS_SERVER_IP` is the host IP **that the scanner connects to**. Set it and `NX_BOSS_ADMIN_IP` to the host's LAN IP for access from the trusted LAN. Set `NX_BOSS_ADMIN_DOMAIN` to the domain on the certificate. The app runs as `NX_BOSS_UID:NX_BOSS_GID`; `config/`, `scans/`, `secrets/cloudflare_api_token`, `caddy-acme/data`, `caddy-acme/config`, and `deploy/Caddyfile` must already exist. Compose refuses missing bind sources instead of creating empty mounts. The `caddy-acme/` directory holds the ACME account and certificate private keys; preserve it through updates. The Cloudflare token needs `Zone.Zone:Read` and `Zone.DNS:Edit` for `maindorfer.name` and is read as a Compose secret. Caddy renews the Let's Encrypt certificate automatically with DNS-01; no inbound internet port is required. Keep the DNS-only A record for `nx.maindorfer.name` pointed at the VM's LAN IP and allow that name through the router's DNS-rebind protection. Docker gives nx-boss 180 seconds to finish a graceful stop before SIGKILL.

The browser URL is `https://nx.maindorfer.name/` on port 443. Scanner HTTP is `http://192.168.178.92:20447`; scanner HTTPS is `https://nx.maindorfer.name:20448`. The fi-7300NX on firmware `0T00` passed its HTTPS connection check with TLS 1.2 `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`. See [scanner TLS research](docs/scanner-auth-tls-research.md). Access by IP causes a certificate name warning.

Ricoh documents port `20447` as the usual scanner connection port for the original NX Manager. The container still listens internally on `10447`; Compose maps both scanner host ports to Caddy, which forwards to that internal listener. [Ricoh network connection FAQ](https://fi-faq.pfu.ricoh.com/hc/en-us/articles/13715321018265-My-scanner-does-not-connet-to-the-network-It-errors-out-even-after-setting-up-network-settings-on-the-scanner-device)

## Scanner API documentation

The scanner HTTP API is described by [OpenAPI 3.1](docs/openapi.json). After setting up the admin account, open `https://nx.maindorfer.name/api-docs` for the local API explorer. It is protected by the admin session and shows schemas and responses without sending write requests. The scanner API is available through Caddy on 20448 (HTTPS) and, in `none` mode, also on 20447 (HTTP). The admin pages use HTML forms and are not a public JSON API.

## Paperless-ngx v3

Create an API token for a dedicated Paperless account with permission to create documents. Configure the URL and token variable in `config/config.yaml`:

```yaml
paperless:
  url: http://paperless-host:8000/api/documents/post_document/
  token_env: PAPERLESS_API_TOKEN
  tag_ids: [1]             # optional existing tag IDs
  batch_id_field_id: 2     # optional text custom field for reconciliation
```

Set `PAPERLESS_API_TOKEN` in the local `.env` file for initial setup, or replace it later on **Settings → Paperless-ngx**. The UI writes each replacement token to a private 0600 file next to `config.yaml` and stores only its path in `paperless.token_file`. Existing environment tokens continue to work until replaced. After a UI rotation, remove the obsolete token value from `.env` and recreate the app container during maintenance; until then the old value still exists in its environment. Back up the active token file; older token files can be removed after verification. Keep `.env` readable only by its owner; users with Docker access can inspect container environment variables. `paperless.url` accepts either the complete upload URL or the base URL. `localhost` inside the container refers to nx-boss, not Paperless. nx-boss sends `Authorization: Token <token>` and places the PDF in multipart field `document`. The UI rejects a target change while a scan or Paperless delivery is pending. Token and tag changes apply live; each running upload uses the client it started with. API delivery can coexist with a job's legacy `consume_path`, but that would submit the same PDF through two paths. Choose one delivery path for a job.

After a batch closes, nx-boss syncs the pages, PDF and an upload-attempt record to disk before sending `POST /api/documents/post_document/`. It records the returned task ID and polls `GET /api/tasks/` for a document ID. After a lost response or restart, it searches tasks by the exact `<batch-id>.pdf` filename before retrying. If Paperless reports no matching task, it retries automatically after a two-minute settle window. New batches persist their tag and batch-ID-field settings before upload so retries keep the same multipart metadata after a configuration change. Pending batches are retained until Paperless confirms a document ID. The admin UI flags unconfirmed imports after one hour and shows their task ID, attempts, and last error. Set `PAPERLESS_CONSUMER_DELETE_DUPLICATES=true` on Paperless when repeated identical PDFs must not create two documents; this affects all Paperless imports. This is best-effort deduplication, not a strict exactly-once guarantee: task history may disappear, another import may already contain the same PDF, or processing may transform bytes before duplicate detection. See [the local recovery guide](docs/paperless-recovery-local.md) and [the delivery plan](docs/security-review.md#paperless-delivery-and-ai).

Paperless itself can classify, tag, and process documents after import. AI integration belongs downstream of the confirmed Paperless document ID; nx-boss currently does not call an AI service. [Paperless API documentation](https://docs.paperless-ngx.com/api/) and [advanced usage](https://docs.paperless-ngx.com/advanced_usage/) describe the current interfaces.

## Scanner protocol

The implemented `NmWebService` flow is heartbeat → device → authorization → scan settings → batch → JPEG image uploads → batch completion. The scanner receives the configured job list during authorization. The admin UI controls `scanner_auth.mode: none|userpass`, username, and password. In `userpass`, the app offers the `auth_type: "userpass"` value measured from the original NX Manager and checks the scanner's JSON fields `auth_info_user` and `auth_info_pass` over HTTPS. The YAML holds only an Argon2id PHC hash with an individual salt; the 32-byte pepper is supplied separately as a private Compose secret. A successful login returns a random bearer token, required for scan routes and revoked on logout, mode change, password change, eight-hour expiry, or restart. Five failed attempts within one minute temporarily block login. Legacy `NX_BOSS_SCANNER_AUTH_MODE`, `NX_BOSS_SCANNER_USERNAME`, and `NX_BOSS_SCANNER_PASSWORD_FILE` remain readable only when no `scanner_auth` block exists in YAML. Migrate with `docker compose run --rm --no-deps nx-boss-rs --config /config/config.yaml --migrate-scanner-auth` while the app is stopped and after backing up the private configuration; verify the scanner before removing the old plaintext file. See [the scanner authentication research](docs/scanner-auth-tls-research.md).

Jobs are configured in YAML and editable in the admin UI, including validated advanced JSON for `job_settings` and scanner `scan_settings`. `resources/defaults.yaml` contains scan settings. A batch stores its metadata and source JPEGs under the job's `output_path`; a completed Paperless batch also stores a staged PDF. `consume_path` remains available for a filesystem-based Paperless deployment.

The job UI does not expose `output_path`: it is an internal storage setting, not a scan choice. Compose sets `NX_BOSS_OUTPUT_PATH=/data` for new jobs. Editing a job preserves its stored path, including paths from older configurations, so existing scans remain discoverable. Outside Compose, new jobs use the first configured job's path or `./scans` if there are no jobs; set `NX_BOSS_OUTPUT_PATH` on the server to choose another location. YAML still supports per-job `output_path` for manual and legacy configurations.

In the Scan Jobs table, drag a row by its handle or focus the handle and use the up/down arrow keys to change the order. A click on the rest of the row opens that job's editor. The order is saved immediately in the config file, appears on the dashboard, and is sent to the scanner on its next authorization request; job IDs do not change. A failed save restores the previous order, and an edit from another browser causes the page to reload before it can overwrite that order.

For protocol experiments, `scripts/test_scan.sh` exercises the local scanner listener. `scripts/rev.py` dumps full requests and responses, so do not use it with real credentials. Share captures only after removing credentials, access tokens, document contents, device serials, and network identifiers.

## Operations

On SIGTERM or SIGINT, nx-boss stops accepting new scanner and admin connections, drains active HTTP requests, lets the current Paperless request and retention operation finish, then joins both workers. It checks for shutdown between queued batches. An unfinished scan stays on disk and is recovered after restart. CI runs `scripts/test_shutdown.py` against the real binary: the test sends SIGTERM during an image upload and completes that batch after restarting. Docker's 180-second stop limit can still interrupt unusually long requests or archives; power loss can also interrupt filesystem writes. A Paperless upload accepted just before a lost response or crash may be submitted again because the task ID has not been recorded yet. Reconcile those cases by batch UUID.

- `GET /health` is available on the scanner listener; the admin UI is on a separate listener.
- A page is limited to 32 MiB, a batch to 100 pages and 256 MiB of JPEG data. Upload concurrency is limited to two and PDF assembly to one.
- Completed batches are eligible for retention only after Paperless confirms their document ID. Set `retention.archive_after_days` and `retention.delete_after_days` in YAML or through the UI.
- Do not delete or move an active job's output directory. Job deletion and output path changes are rejected while that job has pending batches.
- Application logs are JSON lines on stdout. Set `NX_BOSS_LOG_LEVEL=error|warn|info|debug` (default `info`) for the application; arbitrary `RUST_LOG` directives are ignored. HTTP responses include `X-Request-ID`, and request logs contain route patterns, status, and duration. Docker rotates each service's logs after five 10 MiB files. See [logging operations](docs/logging.md) for fields, Grafana ingestion, and privacy rules.

## Security status

The dashboard counts completed documents and pages currently retained on disk, confirmed Paperless imports, and batches needing attention. `/events` displays the latest 200 redacted WARN/ERROR events; Docker JSON logs are the full stream. See the [documentation index](docs/README.md), [architecture](docs/architecture.md), and [operations guide](docs/operations.md).

See [the security review](docs/security-review.md) for mitigations and remaining risks. A new installation uses one Admin account with passkeys and HTTPS through Caddy; setup needs the one-time private token. Administrative writes require a same-origin HTTPS request, and sensitive settings require a fresh passkey confirmation. Existing password/TOTP accounts can still sign in until explicitly replaced; the legacy setup endpoints are disabled by default. Scanner `none` mode remains unauthenticated and the plain HTTP scanner port exposes scan traffic on the LAN. Use scanner HTTPS with `userpass` for credential protection. Limit all published ports to the trusted LAN; do not forward them from the internet. If all Admin passkeys are lost, an operator with filesystem access can stop the app, back up then remove `config/admin-passkeys.json`, restart it, and use the newly generated private setup token to create a new Admin account. This invalidates all Admin sessions. See [Passkey operations](docs/admin-passkeys.md).
