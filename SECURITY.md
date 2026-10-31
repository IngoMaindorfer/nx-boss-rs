# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| latest  | ✅        |

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Use GitHub's private vulnerability reporting instead:
**[Report a vulnerability](https://github.com/IngoMaindorfer/nx-boss-rs/security/advisories/new)**

Include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

You will receive a response within 7 days. If the issue is confirmed, a patch will be released as soon as possible.

## Scope

nx-boss-rs is designed for **trusted internal LAN use only** and should never be exposed to the internet. The relevant threat model is documented in the [README](README.md#security-status).

Admin access requires a WebAuthn passkey. Removed password/one-time-code accounts
are not silently migrated: `config/admin-auth.json` causes startup to stop before
issuing a setup code. Scanner Argon2id credentials and the separate pepper remain.


Optional Admin device trust lasts a fixed 30 days from a verified passkey login.
Only GET page visits can restore a session; sensitive actions still require a
fresh passkey assertion. Tokens stay in a Secure, HttpOnly, SameSite=Strict host
cookie; the private device file contains hashes only. Revoke devices under
`/account`; logout revokes the current device and removing a passkey revokes all.
A missing/corrupt file disables device trust without disabling passkey login.

**Use only your own devices with a lock screen.** Whoever has an unlocked trusted
device can see scans and change jobs and retention periods. Cookie theft also
provides that access until expiry or revocation. Restoring older auth backups can
resurrect revoked credentials, so do not roll back current authentication state.
