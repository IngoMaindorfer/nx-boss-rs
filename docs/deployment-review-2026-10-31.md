# Review-Rollout und Betreiberübergabe, 31.10.2026

## Stand und erster Rollout des Nachtrags

793db1c wurde am **31.10.2026 um 12:47:52 CET (11:47:52 UTC)** auf
`/opt/nx-boss-rs` ausgerollt. Main-CI
[41193945035](https://github.com/IngoMaindorfer/nx-boss-rs/actions/runs/41193945035)
war vollständig grün.

- Image: `sha256:e09b03f4ea79e6f9b718c457f910623e5c30002e13224aaa737781862f433c69`, stabil getaggt als `nx-boss-rs:local`.
- Rollback: `nx-boss-rs:pre-review-20261031` (`sha256:52911e07716de54583420697c23b80b14ae921896345831a06d0c7ce324a300f`).
- Geschützte Sicherung: `/opt/nx-boss-rs/backups/pre-review-20261031/20261031T114733Z/`, Verzeichnis 0700, `source.tar` und `private.tar` 0600; private Inhalte nicht angesehen.
- Vorprüfung 12:47:33 CET: keine Scan-Anfragen in 15 Minuten, kein laufender Export, nächster planmäßiger Export 14:00 CET.
- Abnahme: beide Container healthy, laufendes Image stimmt, HTTP-Health und Admin-HTTPS 200, HSTS und CSP gesetzt, keine neuen ERROR-Events.
- Footer `793db1c`: am unveränderten Binary desselben Images mit isolierter temporärer Konfiguration und virtuellem Test-Passkey bestätigt. Kein produktiver Admin-Zugriff oder Auth-Bypass.

Der Admin ist laut Nachprüfung seit 31.10., 09:50 Uhr eingerichtet.
Der Betreiber muss einen Ersatz-Passkey ergänzen und die Scanner-Zugangsdaten
rotieren. Die möglicherweise aktive Datei `scanner-auth-probe-password` bleibt
unangetastet. Es gab keinen physischen Testscan in diesem Rollout.

## Verbindliches Verfahren für weitere Rollouts

1. Ausschließlich einen Commit von main mit grüner Main-CI verwenden. Mit
   `git archive COMMIT` übertragen und in einem separaten Build-Verzeichnis
   entpacken. Private Laufzeitdateien gehören nicht in den Build.
2. Vor dem Stopp aus redigierten Logs belegen, dass in den letzten 15 Minuten
   keine `/NmWebService/batch`- oder `/NmWebService/image`-Anfragen vorkamen.
   Exportprozesse mit `ps -eo args | grep '[r]estic-export'` prüfen, kein `pgrep -f`
   über SSH. Der nächste Lauf laut PVE-Timer muss mindestens 15 Minuten entfernt
   sein; Startfenster mit bis zu 15 Minuten Zufallsverzögerung berücksichtigen.
3. Image im Build-Verzeichnis mit korrektem GIT_SHA bauen:

   ```sh
   nx_revision=VOLLSTAENDIGER_MAIN_COMMIT
   docker build --target app --build-arg GIT_SHA="$nx_revision" \
     -t "nx-boss-rs:review-$nx_revision" .
   ```

4. Laufendes Image vor dem Wechsel datiert als `nx-boss-rs:pre-THEMA-YYYYMMDD`
   taggen. Nur die App geordnet stoppen. Danach den bisherigen Quellstand und
   private Daten ungelesen in getrennte geschützte Tars unter `backups/` sichern;
   Eigentümer und Rechte erhalten. Sicherung selbst nicht ins Quell-Tar aufnehmen.
5. Versionierte Dateien ohne `--delete` einspielen. Bestehende Verzeichnisrechte
   von `/opt/nx-boss-rs` nicht überschreiben (`tar --no-overwrite-dir`). Entfallene
   Quell-/Template-Dateien gezielt entfernen. `.env`, Inhalte von `config/`,
   `secrets/`, `scans/` und ACME-Daten nicht überschreiben.
6. Neues Image **dauerhaft auf `nx-boss-rs:local` taggen**, dann im aktiven Stack:

   ```sh
   docker image tag "nx-boss-rs:review-$nx_revision" nx-boss-rs:local
   cd /opt/nx-boss-rs
   docker compose up -d --no-build --force-recreate --wait --wait-timeout 180 nx-boss-rs caddy
   ```

   Kein vorübergehendes `NX_BOSS_IMAGE` verwenden: spätere normale Compose-Aufrufe
   müssen denselben neuen Stand starten. `.env` wird durch diesen Auftrag nicht
   verändert. Domain, Ports, Scanner-Modus und Pepper erhalten.
7. Beide Container müssen healthy sein und die App muss die erwartete Image-ID
   verwenden. Health auf HTTP :20447 und Admin-HTTPS, HSTS, CSP, neue ERROR-Events
   und Footer-Revision prüfen. Für einen eingerichteten Admin wird kein virtueller
   Passkey auf Produktion erzeugt; der identische Image-Footer kann isoliert
   geprüft werden, die echte Anmeldung bleibt Betreiberprüfung.
8. Bei fehlgeschlagener Abnahme sofort Rollback: ausschließlich Compose und
   `deploy/Caddyfile` aus dem Quell-Tar zurückholen, das gesicherte pre-Image auf
   `nx-boss-rs:local` taggen und denselben `up`-Befehl ausführen. Aktuelle
   `config/`, `scans/`, Secrets und ACME-Daten **nicht zurückspielen**. Anschließend
   Gesundheit des alten Stands prüfen; bei fehlschlagendem Rollback anhalten.

Jede abgelehnte Sicherheits- oder Berechtigungsprüfung stoppt die Arbeit. Keine
Umgehung oder Zerlegung des abgelehnten Schritts. Paperless, andere Stacks,
Backup-Läufe und Scanner-Rotation sind hier keine Deployment-Aktionen.

## Zweiter Rollout und Betreiberprüfungen

Der zweite Rollout des Nachtrags wird nach grüner Main-CI mit Uhrzeit, Commit,
Image-ID und `pre-trust-YYYYMMDD`-Tag ergänzt. Bei einem späteren Rollback auf
793db1c wird die neue Trusted-Devices-Datei ignoriert; Geräte melden sich wieder
mit Passkey an. Kein älteres Auth-Backup restaurieren, das widerrufene Schlüssel
oder Geräte erneut gültig machen würde.

Danach beim Betreiber:

- Gerätevertrauen mit echtem Passkey und Widerruf unter `/account` prüfen.
- Ersatz-Passkey ergänzen.
- Scanner-Zugangsdaten rotieren (B12), anschließend alte Passwortdatei bewusst entfernen.
- Physischen Testscan bis Paperless 3.4.0 durchführen.
- fi-7300NX nach 401 und nach Neustart mitten im Scan prüfen (B02).
- B07-Konzept entscheiden; dieses verändert den Export noch nicht.
