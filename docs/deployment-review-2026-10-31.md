# Übergabe: Review-Implementierung vom 31.10.2026

Dieser Auftrag bereitet Code und Dokumentation vor. **Kein Deployment**, kein
Lesen oder Ändern produktiver Konfiguration und kein Backup-Lauf wurden dafür
ausgeführt. Der vorherige Wartungsstand auf der VM bleibt davon getrennt.

## Stand je Phase

1. Ausgangsstand veröffentlicht: PR [#19](https://github.com/IngoMaindorfer/nx-boss-rs/pull/19), Merge `44a355d`; alle Branch- und Main-Prüfungen grün. TLS-Advisories behoben, eng begründete RSA-Ausnahme in `.cargo/audit.toml`; Warnungen basic-toml und unsafe-libyaml bleiben sichtbar.
2. Einrichtungscode: Tastatur-Toggle, beim Abschicken wieder verdeckt, getrennte lokalisierte Fehler, Handy/Dark Mode geprüft.
3. Admin nur mit Passkeys. Eine vorhandene `admin-auth.json` beendet den Start ausdrücklich. Scanner-Argon2 und Pepper bleiben erhalten.
4. Scan-/Paperless-Wiederherstellung mit frischer Bestätigung; defekte und alte offene Scans sichtbar; JPEG-Header und CMYK, Job-DPI und Berliner Scandatum korrigiert.
5. Tests benutzen die echten getrennten Router; Login-Grenzen, benannte widerrufbare Passkeys, Journal, Dateirechte, Cache/CSP/HSTS, Token-Aufräumen und Paperless-Pause umgesetzt. B20 bleibt wie entschieden unverändert.
6. Homelab-Startdateien des alten Stacks entfernt, Verweise korrigiert. Homelab bleibt uncommittet. B07 ist ausschließlich ein Vorschlag im Review; B24 bleibt erhalten.

Die Antworten unter jedem Befund im [Review](review-2026-10-31.md) nennen
Code-Commits und Regressionstests. CI/PR-Nachweise werden nach Abschluss dort
und in der Implementierungs-PR verknüpft.

## Deployment durch den Betreiber

1. Nach grüner Main-CI die gewünschte Main-Revision festhalten. Vorher auf der VM
   `/opt/nx-boss-rs` Image-ID/Tag, Compose-Datei und `deploy/Caddyfile` privat für
   Rollback sichern. Einen konsistenten, zugriffsgeschützten Backupstand von
   `config/`, `scans/`, `.env`, `secrets/` und `caddy-acme/` bereithalten; Eigentümer
   und Rechte erhalten. Auf laufende Scans warten. Nicht aus diesem Auftrag
   einen neuen oder veränderten Export ableiten.
2. Ausschließlich versionierte Quellen dieser Revision übertragen, etwa mit
   `git archive`, in ein separates Build-Verzeichnis auf der VM. Dort das Image
   vor dem eigentlichen Stopp bauen; private Laufzeitdateien nicht hineinkopieren:

   ```sh
   # Im entpackten Quellstand; hier den vollständigen gewählten Commit einsetzen.
   nx_review_revision=COMMIT_SHA
   docker build --target app --build-arg GIT_SHA="$nx_review_revision" \
     -t "nx-boss-rs:review-$nx_review_revision" .
   ```

3. Den geprüften Quellstand und seine Compose-/Caddy-Dateien nach dem bisherigen
   Verfahren unter `/opt/nx-boss-rs` installieren, private Dateien erhalten.
   Kein pauschales Löschen/Synchronisieren mit `--delete`. Die alten Templates
   `templates/admin_setup_confirm.html` und `templates/account_totp.html` gehören
   nicht mehr zum Quellstand. Der Build bettet seine Templates bereits ein.
   Den verwendeten Image-Tag privat als `NX_BOSS_IMAGE` für spätere Compose-Aufrufe
   festhalten. Domain, Ports und bisherigen Scanner-Modus erhalten;
   `NX_BOSS_TIMEZONE` ist optional und standardmäßig `Europe/Berlin`.
4. In `/opt/nx-boss-rs` mit dem neuen Image kontrolliert neu erstellen; Caddy muss
   für die neue HSTS-Konfiguration ebenfalls neu starten. Der Caddy-Imagebuild
   selbst hat sich gegenüber Phase 1 nicht geändert:

   ```sh
   NX_BOSS_IMAGE="nx-boss-rs:review-$nx_review_revision" \
     docker compose up -d --no-build --force-recreate --wait --wait-timeout 180 nx-boss-rs caddy
   docker compose ps
   curl --fail --silent --show-error http://192.168.178.92:20447/health
   curl --fail --silent --show-error -I https://nx.maindorfer.name/login
   ```

   Falls `admin-auth.json` existiert, den klaren Startfehler beachten: Datei privat
   sichern und deren Entfernung bewusst als Betreiber entscheiden. Kein
   automatisches Löschen, kein stiller neuer Admin neben dem alten Konto.
5. Footer auf die gewählte Revision prüfen. Browser: `/setup` beziehungsweise
   `/login`, Konto und Wiederherstellung, Cache-Control `private, no-store`, CSP
   `form-action 'self'` und HSTS prüfen. Scanner-Login über HTTPS `:20448`, Jobs
   und harmlosen physischen Scan bis zur bestätigten Paperless-Dokument-ID prüfen.
   Bei pausierter Zustellung `/settings/paperless` korrigieren und speichern;
   keine unkontrollierten Wiederholungen eines unklaren Imports.

## Rollback

Den vorher gesicherten Image-Tag sowie passende Compose-/Caddy-Dateien wieder
verwenden. Im aktiven Stack:

```sh
NX_BOSS_IMAGE=VORHER_GESICHERTER_IMAGE_TAG \
  docker compose up -d --no-build --force-recreate --wait --wait-timeout 180 nx-boss-rs caddy
```

Aktuelle `config/`, Pepper, API-Token, ACME-Daten und insbesondere seit dem Update
entstandene Scans behalten. Kein altes Scanbackup über neuere Daten kopieren.
Neuere Passkey-Namen sind zusätzliche Metadaten; ein alter Stand bietet keinen
Widerruf. Ein ausdrücklich entfernter Schlüssel darf durch Wiederherstellen
alter Auth-Dateien nicht wieder gültig werden. Auf einen Stand vor Einführung
der Admin-Absicherung nur mit gesperrtem Adminzugriff zurückgehen. Health,
Scanner-Neuanmeldung und ausstehende Zustellungen danach prüfen. Das auf der VM
vorhandene Wartungs-Rollback `nx-boss-rs:pre-dependencies-20261031` ist älter als
dieser Review-Ausgangsstand; bevorzugt das unmittelbar vorher gesicherte Image.

## Nur der Betreiber kann noch erledigen

- Eigenen Admin und einen benannten Ersatz-Passkey über `/setup` und `/account` einrichten.
- Scanner-Zugangsdaten rotieren (B12), Altwerte nach Prüfung privat entfernen.
- fi-7300NX nach einer 401 und nach Neustart mitten im Scan physisch prüfen (B02); automatisierte Tests decken die API mit Neuanmeldung ab.
- Harmlosen physischen Testscan bis Paperless 3.4.0 bestätigen.
- B07: Backup-Vorschlag und Messung bei einem freigegebenen Export entscheiden.
- B24: Fortsetzung oder Ende der NFC-Messung entscheiden; Probe-Code bleibt bis dahin.
