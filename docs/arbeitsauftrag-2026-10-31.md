# Arbeitsauftrag nx-boss-rs, 31.10.2026

**Auftraggeber:** Betreiber (Ingo)
**Grundlage:** [review-2026-10-31.md](review-2026-10-31.md). Dort stehen Befunde, Belege und Vorschläge, hier nur Umfang, Reihenfolge und Abnahme.

## Ziel

1. Der Arbeitsstand ist versioniert, veröffentlicht und die CI ist grün.
2. Der Toggle für den Einrichtungscode ist fertig.
3. Der Legacy-Login ist entfernt.
4. Kein Scan bleibt still in einer Sackgasse hängen.
5. Die übrigen Review-Punkte sind abgearbeitet.

## Entscheidungen, die feststehen

- **B01:** Alles darf ins öffentliche Repo, auch `AGENTS.md`, `docs/` und die enthaltenen LAN-IPs, Domains und Pfade. Nichts davon bereinigen. Secrets bleiben ausgeschlossen.
- **B13:** Paperless bekommt das Scandatum. `created` bleibt.
- **B20:** Kein zusätzlicher Step-up, nicht umsetzen.
- **B23:** Passwort/TOTP vollständig entfernen, auch die spätere TOTP-Option entfällt.

## Rahmen

- `AGENTS.md` gilt: zuerst ein fehlschlagender Test, keine Secrets in Ausgaben oder Doku, `.env`, `config/`, `secrets/` und `scans/` nicht lesen.
- Der Arbeitsbaum enthält fremde, parallele Änderungen. Nichts zurücksetzen oder pauschal überschreiben.
- **Commit und Push sind freigegeben**, auf Branches und nach `main` ohne Force-Push, sobald die CI des Branches grün ist.
- **Nicht freigegeben:** Deployment auf die VM, Änderungen an produktiver Konfiguration und Änderungen am laufenden Backup.
- Im Repo `homelab` nur Dateien bearbeiten, dort nicht committen. Es enthält eigene uncommittete Änderungen.

## Sofort anhalten und nachfragen, wenn

- der Secret-Scan einen echten Treffer liefert. Dann nichts pushen. Einmal veröffentlicht, lässt sich das nicht zurückholen.
- eine Datei in der Commit-Liste auftaucht, die unter `config/`, `secrets/`, `scans/`, `backups/` oder `.env` fällt.
- eine Korrektur die Backup-Konsistenz oder das Scanner-Protokoll betrifft und sich nicht ohne Annahmen testen lässt.

## Phase 1: Stand sichern (B03, B01)

Keine Verhaltensänderungen in dieser Phase, damit alle späteren Diffs sauber lesbar sind.

1. **B03:** `cargo update -p rustls -p rustls-webpki -p webpki-root-certs`. Für `rsa` (Marvin) den Pfad in `webauthn-rs` prüfen und eine `audit.toml` mit begründeter Ausnahme anlegen. Trifft die Annahme nicht zu, nachfragen.
2. **Secret-Scan:** gitleaks oder gleichwertig über alle zu committenden Dateien. Das Dummy-Token in `scripts/test_caddy_dns.sh` und in der CI ist bekannt.
3. **Commits:** Branch anlegen, thematisch getrennte Commits, vor jedem `git add` die Liste prüfen. Kein pauschales `git add -A` ohne Sichtkontrolle.
4. `GIT_SHA` in Compose und beim Build übergeben, damit der Footer nicht mehr `dev` zeigt.
5. Pushen, CI grün bekommen, nach `main` bringen. Die CI läuft hier zum ersten Mal über diesen Stand, Fehler dort gehören zu dieser Phase.

**Abnahme:** `main` entspricht dem Arbeitsstand, alle CI-Jobs sind grün einschließlich `audit`.

## Phase 2: Einrichtung ermöglichen (F1, F1b)

Umsetzung nach Review-Abschnitt F1, die Tabelle der Fallen dort ist verbindlich. Dazu F1b mit drei getrennten Fehlermeldungen.

**Abnahme:** neuer Browser-Test in `passkey.spec.js`, Sichtprüfung auf Handybreite und im Dark Mode, Deutsch und Englisch.

## Phase 3: Legacy-Login entfernen (B23)

**Entfernen:**

- in `admin_auth.rs`: Passwort- und TOTP-Teile, also `AdminCredentials`, `PendingSetup`, `PendingTotp`, `SetupPreview`, `totp`, `prepare_setup`, `complete_setup*`, `login*`, `reauthenticate*`, `update_account*`, `*_totp_rotation*`, `persist`, Fehlversuchszähler
- Routen `POST /login`, `/setup/prepare`, `/setup/complete`, `POST /account`, `/account/totp/*`
- `admin_setup_confirm.html`, `account_totp.html`
- die Legacy-Zweige in `admin_login.html`, `account.html`, `scanner_security.html`, `paperless_settings.html`
- die Formularfelder `admin_password` und `admin_code`
- `NX_BOSS_LEGACY_ADMIN_SETUP`
- die Abhängigkeiten `totp-rs` und `base32`

**Behalten:** Argon2 und Pepper für den Scanner-Login, ebenso `hash_secret` und `pepper()`.

**Dabei beachten:**

- Liegt beim Start eine `admin-auth.json` vor, mit klarer Fehlermeldung abbrechen. Sie darf nicht still ignoriert werden, sonst entstünde ein neuer Setup-Code neben einem alten Konto.
- `scripts/bootstrap_browser_test.py` und `ui.spec.js` hängen am Legacy-Setup und müssen auf den virtuellen Authenticator umgestellt werden.
- Rust-Tests wie `configured_admin_requires_session_and_logout_revokes_it` brauchen einen reinen Test-Weg zu einer Sitzung, der nicht im Produktionsbinary landet.
- Der erste Punkt von B22 entfällt damit. Doku nachziehen: `admin-passkeys.md`, `security-review.md`, `SECURITY.md`, README.

**Abnahme:** `rg -i 'totp|legacy_admin'` findet in `src/`, `templates/` und `Cargo.toml` nichts mehr. Alle Browser-Tests laufen über Passkeys.

## Phase 4: Sackgassen schließen (B02, B11, B04, B10, B13)

- **B02:** alle vier Vorschläge aus dem Review. Die Aktionen „Abschließen und zustellen“ und „Verwerfen“ verlangen Step-up und funktionieren auch nach dem 24-Stunden-Sweep.
- **B11:** `parse_jpeg` beim Upload prüfen, Füllbytes, CMYK, Job-Auflösung als Fallback. Testdaten mit `FF D8 FF D9` durch echte JPEGs ersetzen.
- **B04:** „Erneut senden“ mit Duplikat-Warnung und „Dokument-ID zuordnen“, beides mit Step-up. Metadaten nur bei Zustandsänderung schreiben.
- **B10:** defekte Batches als eigener Zustand, klare Meldung statt „offener Upload“.
- **B13:** nur die Zeitzone. Datum im Feld `created` und im Titel aus lokaler Zeit bilden, `created_at` in den Metadaten bleibt UTC. Das Image basiert auf `scratch` und hat keine Zeitzonendaten, die Zeitzone also explizit konfigurieren, Standard `Europe/Berlin`. Test mit 22:30 UTC im Sommer und 23:30 UTC im Winter.

**Abnahme:** `test_shutdown.py` deckt auch `userpass` mit Neu-Anmeldung ab, ein Test belegt den leeren Batch mit aktivem Paperless.

## Phase 5: Absichern (B05, B06, B09, B14 bis B19, B21, B22, B25)

- **B09 zuerst.** Danach laufen die Routentests gegen die echten Router, und alle weiteren Änderungen profitieren davon.
- **B05, B06:** nach Review-Vorschlag. Beide mit Tests, Entfernen eines Passkeys mit Step-up.
- Die Niedrig-Punkte einzeln oder gebündelt, je mit eigenem Test, soweit sinnvoll.
- **B25** zuletzt, da B23 einen Teil davon schon erledigt.

## Phase 6: Homelab (B08, B07)

- **B08:** veraltete Compose-Datei und Beispielkonfiguration in `docker/nx-boss/` entfernen oder auf `/opt/nx-boss-rs` verweisen, Tabellen in `README.md` und `docker/README.md` korrigieren.
- **B07:** nur Vorschlag ausarbeiten und im Review unter B07 eintragen, nicht umsetzen. Inhalt: was der Stopp absichert, ob ein Lauf ohne Stopp konsistent ist, welche Änderung am Exportskript nötig wäre. Die Änderung gibt der Betreiber frei.

## Nicht Teil dieses Auftrags

- **B20:** entschieden, keine Umsetzung.
- **B24 (Probes):** bleibt, bis der Betreiber zur NFC-Messung entschieden hat.
- **Deployment:** Am Ende nur die Schritte und das Rollback auflisten.

## Definition of Done

Vor jedem Merge nach `main`:

```sh
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
NX_BOSS_REQUIRE_HTTP_TESTS=1 cargo test --locked
cargo audit
docker compose config --quiet
python3 scripts/test_openapi.py
python3 scripts/test_compose_boundary.py
python3 scripts/test_caddy_mounts.py
npm run test:e2e
```

Bei Änderungen am Laufzeitverhalten zusätzlich `scripts/test_shutdown.py` und `scripts/test_logging.py`.

Zusätzlich:

- Im Review ist unter jedem bearbeiteten Punkt das Antwortfeld ausgefüllt, mit Test-Name und Commit. Der Befundtext bleibt unverändert.
- Doku entspricht dem Stand, ein neuer Eintrag steht oben in `AGENTS.md`.
- Ergebnisse werden so berichtet, wie sie sind, auch fehlgeschlagene oder übersprungene Prüfungen.

## Liefergegenstand am Ende

Eine kurze Übergabe an den Betreiber mit:

1. dem Stand je Phase
2. den Deployment-Schritten mit Rollback
3. dieser Liste, die nur der Betreiber erledigen kann:
   - Admin und Ersatz-Passkey auf `/setup` einrichten
   - Scanner-Zugangsdaten rotieren (B12)
   - physischer Test: Verhalten des fi-7300NX nach 401 und Neustart mitten im Scan (B02)
   - physischer Testscan bis Paperless 3.4.0
   - Entscheidungen zu B07 und B24
