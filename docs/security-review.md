# Security review and next steps

Status: 2026-10-31 (review implementation, deployment pending). This records code changes and decisions for the scanner and Paperless deployment. A green automated test suite does not establish compatibility with a physical scanner or an authenticated Paperless instance.

## Findings addressed with regression tests

| Area | Failure mode | Change |
| --- | --- | --- |
| Page upload | Path traversal, reserved names, changed retry, oversized batches | Validate JPEG basenames; accept identical retry; reject conflicts; cap page, batch, and request size. |
| Multipart | Parser error after valid fields could be treated as success | Return 400 on every parser or field read error before storing a page. |
| Batch lifecycle | Completion marked before output was delivered; concurrent requests raced | Serialize operations per batch, write delivery files through temporary paths, mark complete last, recover open batches after restart. |
| Job/config changes | Job IDs shifted; advanced settings disappeared; failed save published state | Stable IDs, preserve raw settings, atomically replace config, publish state only after persistence, block deletion or path changes while work is pending. |
| Scan UI | Any file under a batch directory could be served as JPEG | Serve only filenames listed in batch metadata; reject symlink page files. |
| Browser/admin | Shared listener, unauthenticated access, cross-origin writes, DNS rebinding | Separate admin port; passkey setup and sign-in for new installs; private sessions; matching HTTPS Origin; allowed Host headers; local scripts and browser security headers. Only passkeys are accepted; an obsolete admin-auth.json stops startup. |
| Scanner credentials | Private plaintext file and unlimited async hash work | Argon2id hash with random salt and separate pepper, bounded blocking verification, mode-change session revocation; legacy file retained only for rollback. |
| Paperless configuration | Token in process environment and target changes during uncertain delivery | Private replaceable token file, live client swap, target-change guard while scans or imports are pending. |
| Paperless retention | Pending import might be archived or deleted | Retain a Paperless batch until a document ID is confirmed. |
| Retention archive | A failed compression left a published partial archive | Build in a temporary file, sync it, and publish by rename only after compression succeeds. |
| PDF dependency | `lopdf 0.42` pulled in unmaintained `ttf-parser` although nx-boss embeds no fonts | Upgrade to `lopdf 0.46` with default features disabled; font parsing is absent from the lockfile. |
| Resource exhaustion | Large/many simultaneous uploads | Two upload slots, one PDF assembly slot, page/batch/request limits. |

The admin UI now supports one local passkey account for new installations. The one-time setup code lives only in the private config volume. Sessions use Secure/HttpOnly/SameSite cookies; writes require a matching HTTPS Origin and sensitive settings require a fresh passkey assertion. An existing admin-auth.json is rejected explicitly; it is never ignored. Scanner `userpass` verifies a salted, peppered Argon2id hash and issues a bearer over HTTPS. Scanner `none` remains available and unauthenticated. The default host Compose bindings are loopback; the VM publishes only Caddy on its trusted LAN IP. Caddy obtains and renews a publicly trusted certificate with Cloudflare DNS-01. Do not expose either listener to an untrusted network.

## Scanner deployment and protocol validation

The device model is confirmed as `fi-7300NX`. The current deployment uses Ricoh's **Username/Password** authentication mode ([Ricoh FAQ](https://fi-faq.pfu.ricoh.com/hc/en-us/articles/13717467155737-What-authentication-methods-do-scanner-device-have-and-how-do-I-configure-it)). An original NX Manager installation is available on a Windows VM, so it can serve as a protocol reference. [Scanner authentication and HTTPS research](scanner-auth-tls-research.md) records supported device modes, the distinction from HTTP Basic, certificate uncertainty, and the physical-device checks still needed after deployment.

The original Windows VM's `NmWebService` has an HTTP binding on port `20447`. Scanner HTTP remains published on the Docker VM's port `20447`, but `userpass` login and protected scan routes require HTTPS and reject that path. The `none` mode, if selected, would allow unauthenticated LAN scans. Caddy serves admin HTTPS on `443` and scanner HTTPS on `20448`. Firmware `0R00` offered only TLS 1.2 `TLS_RSA_WITH_AES_128_CBC_SHA256`, which Caddy rejected. After the user's `0T00` update, the physical scanner successfully checked its HTTPS connection; a passive handshake showed Caddy selecting TLS 1.2 `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`. Whether the scanner's new server-certificate-verification option is enabled and whether a full HTTPS scan succeeds remain to be checked. See [the measured handshake and firmware notes](scanner-auth-tls-research.md). Do not expose these ports through internet forwarding.

### Historical first session with the Windows VM

1. Put the VM and fi-7300NX on a LAN where the scanner can reach the VM. A VM in NAT mode needs port forwarding from the LAN; a bridged VM normally has its own LAN address.
2. In the NX Manager admin UI, create an authentication entry using **Keine**, a disposable scan job, and a job group containing that job. The guide places these under **Authentifizierung**, **Auftrag**, and **Auftragsgruppe**. Use a harmless test page and a temporary output destination. [Ricoh NX Manager user guide](https://origin.pfultd.com/downloads/IMAGE/manual/nxmanager/P2WW-3940-18DEZ2.pdf)
3. On the scanner, choose **Netzwerk → Netzwerkverschlüsselung → Verschlüsselung aus (HTTP)**. Under **Netzwerk → Server → PaperStream NX Manager**, select a private server and set the VM's LAN address and scanner connection port `20447`. Use **Überprüfen Sie die Verbindung**. If it cannot connect, check the VM firewall and whether the NX Manager IIS site/application pool and SQL Server service are running. [fi-7300NX operator guide](https://origin.pfultd.com/downloads/IMAGE/manual/fi-7300nx/P3PC-6192-08DEZ2.pdf), [Ricoh connection FAQ](https://fi-faq.pfu.ricoh.com/hc/en-us/articles/13715321018265-My-scanner-does-not-connet-to-the-network-It-errors-out-even-after-setting-up-network-settings-on-the-scanner-device)
4. Confirm that the job appears on the scanner and one harmless test page reaches the VM. Record the NX Manager version, scanner firmware version, and any displayed error code. This confirms the reference setup before testing nx-boss.

If a protocol trace is needed, stop capture after the job list appears and before scanning a real document. A raw packet capture or `scripts/rev.py` dump may include access tokens, device identifiers, and scanned content; keep it local until reviewed and redacted.

The current nx-boss scanner endpoint is Caddy HTTPS on port `20448` with `auth_type: "userpass"`. The historical HTTP/`none` experiment above is retained as protocol background.

Run nx-boss with `NX_BOSS_LOG_LEVEL=debug` for the experiment. The authorization handler does not log request fields or values. JSON access logs contain registered route patterns and generated request IDs, never raw paths, query strings, headers, or bodies; device serial and IP are not written to the registration log. Add a failing regression test for each observed protocol mismatch, implement the smallest fix, then rerun the full suite and lint.

Protocol-native scanner login and HTTPS are implemented. A synthetic login through Caddy using the private VM test credentials succeeds with a real bearer and job list. Physical login and one harmless scan with the migrated hash remain the next acceptance test.

## Paperless delivery and AI

The HTTP API path posts a staged PDF and persists the returned task UUID, then polls for a confirmed document ID. For Paperless API v10, a successful task must report exactly one positive ID in `related_document_ids`; malformed or multiple references leave the batch pending with an error for investigation. `failure` and `revoked` are terminal errors, while `pending` and `started` keep waiting. Legacy uppercase statuses and `related_document` remain supported. API tokens are loaded from an environment variable or a private token file replaced through the Admin UI; redirect following is disabled so a redirect cannot forward the token to another origin. Use TLS or a trusted private container network between nx-boss and Paperless. Scope the token to document creation and task lookup as far as Paperless permissions allow.

For API delivery, nx-boss syncs the page files, staged PDF, and an upload-attempt record before the POST. If the response or subsequent task-ID write is lost, the worker searches Paperless tasks by the exact `<batch-id>.pdf` filename. A found task is adopted; if none is visible, the worker waits at least two minutes from the last attempt and automatically retries. Incomplete task-search responses prevent a retry. Paperless v3 permits duplicate content by default; set `PAPERLESS_CONSUMER_DELETE_DUPLICATES=true` to reject identical PDFs across **all** import sources. This reduces duplicate documents after a lost response, but it also prevents intentionally importing identical documents and is not a formal exactly-once protocol. Task retention, delayed indexing, and processing that changes bytes can still leave ambiguity. API delivery and the optional legacy consume-folder delivery should not be enabled together for one job; the latter has no task-ID handshake and still needs separate recovery design.

Do not configure both `consume_path` and the API for the same job unless duplicate ingestion is intentional. API delivery and native Paperless classification are separate from a future AI workflow. Add any AI stage after the task produces a document ID, with separate credentials, a documented privacy policy, bounded retries, and tests for repeated callbacks and document updates.

## Remaining engineering risks

- A batch can hold 256 MiB of JPEG data and PDF assembly currently loads all pages into memory. The semaphore bounds concurrency, but a single large batch can still require substantial RAM. Measure on the target host and lower limits or stream assembly before increasing throughput.
- Local users with write access to scan storage can alter metadata and files. Treat the volume as sensitive and grant write access only to the service. A malicious writer may still race a file check; stronger protection would use descriptor-relative, no-follow file operations.
- Atomic renames protect individual metadata and PDF files; a power loss can still leave the remote upload outcome uncertain. Monitor pending and error states and back up config and scan storage.
- The container process runs as the configured non-root UID/GID in a restricted, read-only filesystem with dropped capabilities. Protect bind-mount ownership and permissions as well as Docker access.
- An Admin who loses every enrolled passkey needs a filesystem-backed reset by the operator; one-time recovery codes are not implemented. Enroll a second authenticator first. The event browser contains only bounded, redacted event codes and safe IDs; stdout JSON logs are the full operational stream. Retain and protect both according to document sensitivity.
- Backup/restore and physical scanner regression tests remain manual operational gates. The current dashboard statistics cover stored batches, not lifetime totals.
- Automatic scanner compatibility and authenticated Paperless end-to-end tests require a real test device or service credentials. CI runs protocol simulations, API mocks, listener isolation checks, lint, dependency audit, and a container build.

## TDD sequence for further work

For each change, add a failing test that demonstrates the observed risk, make the smallest implementation change, run the targeted test, then run the full suite and lint. Prioritize: scanner protocol mismatches found with the physical device; operator reconciliation for ambiguous Paperless uploads; resource usage under the maximum batch; descriptor-safe file access; non-root container operation; physical scanner and Paperless staging tests.


The 31 October review implementation adds independent bounded login/confirmation
pools, per-client login-start limits, named removable passkeys with session
revocation, private/no-store admin responses, same-origin-only form destinations,
and HSTS on Caddy TLS listeners. Tests now exercise the separate production
routers. Recovery actions require fresh one-use passkey confirmation. See the
[review answers](review-2026-10-31.md) for per-finding tests and commits and the
[deployment handoff](deployment-review-2026-10-31.md) for operator-only checks.


## Optional trusted Admin devices (31 October addendum)

A verified passkey login can explicitly enroll a device for a fixed 30 days.
A Secure/HttpOnly/SameSite=Strict host cookie carries the random bearer; only its
SHA-256 hash is persisted in a private atomic file. GET page visits alone restore
normal sessions, with no step-up. Existing passkey-confirmed actions retain that
requirement; B20 stays rejected. Revoking a device ends associated sessions,
removing a passkey revokes all devices, and logout revokes the current device.
Missing/corrupt device state falls back to passkey login with a redacted warning.

An unlocked trusted device permits reading scans and changing jobs and retention
periods. Recommend enrollment only on personal devices with a lock screen. This
residual risk is inherent in the explicitly requested convenience feature; the
fixed lifetime is not extended by use and cannot be configured. A copied bearer
cookie must be revoked under `/account`. Backup restoration must not silently
reintroduce previously revoked authentication state.
