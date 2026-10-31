# nx-boss documentation

- [Architecture and delivery](architecture.md): boundaries, batch lifecycle and recovery.
- [Scanner API (OpenAPI 3.1)](openapi.json): machine-readable contract; after admin login, browse it at `/api-docs` and download it from `/api/openapi.json`.
- [Operations](operations.md): deployment, checks, backups and incident response.
- [Admin passkeys](admin-passkeys.md): enrollment, passkey confirmation, recovery and test boundaries.
- [Logging](logging.md): structured log fields and Grafana ingestion.
- [Scanner authentication and TLS](scanner-auth-tls-research.md): observed fi-7300NX behavior and unresolved login work.
- [Web configuration design and implementation notes](config-web-ui-design.md): application settings, password hashing, secrets, live updates, migration and TDD gates.
- [Paperless local recovery](paperless-recovery-local.md): pending/ambiguous imports.
- [Security review](security-review.md): tested protections and remaining risks.
- [Exactly-once analysis](paperless-exactly-once-analysis.md): limits of the Paperless v3 API.

The VM runtime files (`.env`, `secrets/cloudflare_api_token`, `secrets/auth_pepper`, `config/`, `scans/`, `caddy-acme/`) are private deployment state. Repository examples are not a substitute for the VM's live configuration.
