# Operations on the Docker VM

The live project is `/opt/nx-boss-rs` on SSH alias `docker`. Its one `docker-compose.yml` starts nx-boss and Caddy. Preserve `.env`, `secrets/cloudflare_api_token`, `secrets/auth_pepper`, `config/`, `scans/`, and `caddy-acme/` during upgrades. Caddy issues and renews the public certificate with Cloudflare DNS-01. Do not run `docker compose down -v` on production data.

## Project layout

Keep the root for manifests and entrypoints. `deploy/Caddyfile` holds the proxy configuration, `examples/config.yaml` is the copyable sample, and `resources/defaults.yaml` is embedded in the Rust binary. Keep the Cloudflare token and 32-byte auth pepper in private files under `secrets/` and other live state in its ignored mount paths. Dated rollback files belong under private `backups/`. The old root-level VM backups were archived as `backups/legacy-root/`; older historical notes may still quote their former paths. `backups/` and `secrets/` are excluded from both Git and the Docker build context.

## Health and routing

```sh
ssh -F /home/ingo/.ssh/config docker 'cd /opt/nx-boss-rs && docker compose ps'
curl --fail --silent --show-error https://nx.maindorfer.name/login
curl --fail --silent --show-error http://192.168.178.92:20447/health
```

The scanner HTTPS port is `20448`. Firmware `0T00` passed “Verbindung prüfen” there with TLS 1.2 and `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`; the earlier `0R00` reported `M4:A7` because its handshake ended before any certificate. A physical login and scan with the hashed credentials succeeded on 2026-10-01; see [scanner research](scanner-auth-tls-research.md). A browser receiving a DNS error for `nx.maindorfer.name` may be using its own DNS-over-HTTPS resolver. Check that resolver or add a per-site exception; the local DNS A record points to the VM's LAN IP.

## First admin setup and scanner credential rotation

1. On the operator's own workstation, read the private one-time code with `ssh -F /home/ingo/.ssh/config docker 'cat /opt/nx-boss-rs/config/admin-setup-token'`. Do not paste the code into tickets, chat, logs, or shell history. Open `https://nx.maindorfer.name/setup`, choose an Admin name, and register a Passkey using the device's PIN or biometric confirmation. Successful setup deletes the one-time token. Under **Settings → Admin account**, register a second passkey on another device as recovery. See [Passkey operations](admin-passkeys.md).
2. While logged in, open **Settings → Scanner authentication** (`/settings/scanner`). Keep `userpass`; replace the disposable scanner username and password with a unique scanner-only account. The form requires a fresh Admin passkey confirmation and revokes all scanner sessions. Use HTTPS on port `20448` at the device, update its saved Auto Login username/password, then sign in again and confirm the jobs are visible. A harmless scan verifies the new credentials end to end.
3. After that verification, remove the obsolete `NX_BOSS_SCANNER_AUTH_MODE`, `NX_BOSS_SCANNER_USERNAME`, and `NX_BOSS_SCANNER_PASSWORD_FILE` values from the private VM `.env` and remove the old private test password file. The YAML hash and separate pepper are the active source. Recreate only nx-boss so its process environment no longer contains the legacy values, then verify `auth_type:userpass`, scanner login, and container health. Keep any rollback material in private `backups/`, never in Git or the Docker build context.

## Logs and imports

Use `docker compose ... logs --since 1h nx-boss-rs caddy` for the JSON application log and proxy events. Application event names, levels and request IDs are described in [logging](logging.md). The admin `/events` page contains a private, bounded, redacted WARN/ERROR journal; it is a recent incident aid, not a replacement for Docker or Grafana retention. Its persistence warning means the journal cannot be written. The dashboard's Paperless attention count links to affected batches; use [the recovery guide](paperless-recovery-local.md) before manually retrying an ambiguous upload.

## Backups and rollback

Back up `config/`, `scans/`, `.env`, `secrets/cloudflare_api_token`, `secrets/auth_pepper`, and `caddy-acme/` to access-controlled storage. `config/admin-passkeys.json` holds the Admin public key records and is part of `config/`; keep its mode 0600. Keep the pepper separately from the YAML backup; without it scanner password hashes cannot be verified. Include `config/paperless-token-*` after replacing a Paperless token in the UI. Include file modes and ownership, and rehearse a restore in an isolated Compose project before treating the deployment as production-ready. A rollback must keep current scan data and the Paperless token. Restore the previously tagged image and matching Compose/Caddy files; do not restore an old `scans/` snapshot over newer batches. An older image cannot read `admin-passkeys.json`; do not expose the Admin listener after such a rollback until its authentication state is deliberately restored. Check both health endpoints and a known existing scan after the rollback.

## Deployment readiness

The Compose stack has private pre-created bind mounts, a read-only application root, non-root UID/GID, dropped capabilities, a healthcheck, restart policy, bounded JSON logs, and a 180-second stop grace period. Admin passkeys and scanner `userpass` are implemented. On 2026-10-01, a physical fi-7300NX scan after the hash/pepper migration produced a completed two-page batch and a Paperless document confirmed by an authenticated read-only API GET; see [the scanner test record](scanner-auth-tls-research.md). The Admin passkey still needs to be enrolled by the operator, and disposable scanner credentials still need rotation. The project is intended for LAN-only deployment. Paperless delivery has no formal exactly-once guarantee. A power cut can still interrupt filesystem writes or leave an upload outcome ambiguous.

## Reproducible build identity

Build from a committed revision with `GIT_SHA=$(git rev-parse HEAD) docker compose build nx-boss-rs`.
Compose forwards the revision to Docker; GitHub CI and releases pass `github.sha`.
An unspecified local build is labelled `unknown`, never claimed to be a committed revision.
The security-audit exception and its limited public-key-only RSA path are documented
in `.cargo/audit.toml`. The remaining unmaintained dependencies are `basic-toml`
(build-time Askama parser) and `unsafe-libyaml` (runtime YAML parser via serde_yaml);
they are warnings, not silently ignored advisories. The yanked yoke-derive was updated.

The scan detail page now offers passkey-confirmed completion/discard and Paperless
retry/manual assignment. See [recovery actions](paperless-recovery-local.md#recovery-actions-added-by-the-31-october-review).
Use `NX_BOSS_TIMEZONE=Europe/Berlin` (default) to form the Paperless scan date/title;
this does not change UTC timestamps in scan metadata. The shutdown integration
check now explicitly covers `userpass`, rejects the old session after restart,
and logs in again before completing the recovered batch. Actual fi-7300NX behavior
after a 401 still requires the operator's physical test.


## Changes ready for operator deployment, 31 October 2026

[Deployment and rollback handoff](deployment-review-2026-10-31.md) records the
review implementation. This work does not deploy or modify VM configuration.
Admin HTML and images use `private, no-store`; CSP limits form submissions to
self, and Caddy sends HSTS on its TLS listeners. Scanner device registration has
a 16 KiB body limit and bounded fields; its observed unauthenticated wire contract
is unchanged. Retention archives and sidecars are created with mode 0600.

Missing/invalid Paperless credentials at startup pause API delivery while local
scanning continues. `/settings/paperless` displays the paused state; correct and
save the token there to resume. Previously generated token files are removed only
after the replacement configuration is durable; disabling also removes the owned
old token file. External token files and old backup copies are not deleted, and
revoking the actual Paperless API token remains an operator action.
