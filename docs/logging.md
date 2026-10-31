# Logging and collection

nx-boss writes one JSON object per line to stdout. `timestamp` is UTC, `level` and `target` identify severity and Rust module, `message` is a fixed description, and `event` is a stable machine-readable code on operational events. HTTP completion events use `event=http.request`, `request_id`, `http_method`, `http_route`, `http_status`, and `duration_ms`. `http_route` is the registered pattern such as `/scans/{id}`; unknown paths become `<unmatched>`. Successful `/health` probes are omitted to control volume. `X-Request-ID` on every HTTP response lets an operator find the matching request log; handler logs also inherit a request span with that ID. Paperless delivery failures include `error_class` and numeric `http_status` (0 if no HTTP response was received).

Example (identifiers and timing are illustrative):

```json
{"timestamp":"2026-09-30T12:00:00Z","level":"INFO","target":"nx_boss_rs::routes","event":"http.request","request_id":"67d9f0a41234567890abcdef12345678","http_method":"GET","http_route":"/jobs/{id}/edit","http_status":200,"duration_ms":3,"message":"request completed"}
```

Set `NX_BOSS_LOG_LEVEL` to exactly `error`, `warn`, `info`, or `debug`; other values fall back to `info` without echoing the supplied value. `RUST_LOG` is ignored. Keep production at `info` unless investigating a problem. Each container uses Docker's `json-file` driver with `max-size=10m` and `max-file=5` (roughly 50 MiB per service). `docker compose logs --no-color nx-boss-rs` displays the JSON payload with a Compose prefix. A future Grafana Alloy/Loki collector can read the Docker log stream, unwrap the Docker `log` field, parse the application JSON, and use `service`, `level`, and `event` as low-cardinality labels. Keep `request_id`, `batch_id`, route, and duration as structured fields, not index labels. No collector or remote log shipping is enabled by this change.

Application code must not log tokens, passwords, headers, URL paths or queries, request/response bodies, job names, filenames, output paths, device identifiers, or raw error strings. Log fixed event codes and coarse error classes instead. Batch IDs are logged only if they have the generated 32-character hex form. Do not add `Debug` output of whole request or config objects. Logs can still reveal when scans happened and how many pages they had; restrict log access and retention accordingly. Historical logs from earlier versions may contain job names, filenames, and paths until Docker rotation removes them.

The temporary scanner protocol diagnostic and its flags were removed after the operator ended NFC measurement on 2026-10-31. Only fixed login/scan events remain; request values and credential-match metadata are not collected.

CI launches the actual binary, exercises scanner and admin routes with synthetic secrets, checks every line is JSON, verifies `X-Request-ID` correlation, and rejects those secrets anywhere in the log. This test is a regression gate for covered flows; review new log call sites when adding features.


The private `/events` journal keeps at most 200 distinct redacted incident groups.
An identical level/event/batch/error/status increments `count` and updates
`last_seen`, retaining the first timestamp. The group moves to the newest position.
A bounded background notification queue coalesces persistence (200 ms cadence);
request tracing only updates memory. Shutdown explicitly flushes the journal.
A crash can lose the most recent unflushed journal changes; Docker JSON logs
remain the full stream. Journal persistence failure is visible in the UI and
does not prevent scanning or a clean shutdown.
