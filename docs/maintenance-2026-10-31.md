# Wartung 31.10.2026

nx-boss ist der Scannerdienst, der aktualisiert werden sollte. Die zuvor im
Homelab installierte Oxidoc-Instanz wurde auf Nutzerwunsch gestoppt; App und
Datenbank haben in Docker und Compose `restart: "no"`. Ihre zusätzliche Caddyroute
wurde entfernt. Daten, Quellen und Schlüssel bleiben erhalten. Der Backup-Exporter
versucht keine Dumps der gestoppten Oxidoc-Datenbank.

## Abhängigkeiten und Build

Rust 1.99.1 über `rust-toolchain.toml` und Docker-Builder festgelegt. Direkte
Abhängigkeiten mit der crates.io-API gegen neueste stabile Versionen verglichen.
Aktualisiert: Tokio 1.54.1, serde_json 1.0.154, UUID 1.29.0, lopdf 0.46.0,
zstd 0.14.3, getrandom 0.4.3 und WebAuthn 0.6.2; transitive Versionen in Cargo.lock.
`serde_yaml` bleibt bei der letzten verfügbaren Version 0.9.34 (upstream deprecated).
Bestehende API-Verwendung ist mit WebAuthn 0.6.2 kompatibel.

Für Clippy 1.99 werden Fehlerantworten des Multipart-Parsers intern geboxt;
HTTP-Status und JSON-Inhalt bleiben gleich. Docker nutzt Registry-/Target-Caches,
kopiert das fertige statische Binary aber außerhalb des Cachemounts in den
Runtime-Stage. Runtime bleibt Scratch, unprivilegiert und ohne veröffentlichte
Appports; Caddy bleibt die einzige veröffentlichte Grenze.

Image `nx-boss-rs:20261031`, produktiv zusätzlich als `nx-boss-rs:local` getaggt.
Image-ID: `sha256:52911e07716de54583420697c23b80b14ae921896345831a06d0c7ce324a300f`.
Der laufende Container wurde gegen diese Image-ID geprüft.

## Validierung

- `cargo check --all-targets`, fmt und Clippy `--locked --all-targets -- -D warnings` erfolgreich.
- `NX_BOSS_REQUIRE_HTTP_TESTS=1 cargo test --locked`: 223 erfolgreich.
- Browser: 14 Playwright-Tests erfolgreich, einschließlich isoliertem Passkey-Setup,
  zweitem Authenticator, Login, Replay-/Origin-Abwehr, Bestätigung sensibler Änderungen,
  Neustart und Scanner-Passwortmodus. Keine virtuellen Passkeys in Produktion angelegt.
- SIGTERM während Upload: Upload geleert, Batch nach Neustart wiederhergestellt und
  abgeschlossen. Loggingtest: 32 JSON-Zeilen, Korrelation und Secret-Redaktion erfolgreich.
- OpenAPI-Vertrag, Compose-Portgrenzen und Caddy-Mountchecks erfolgreich.
- Isolierter Compose-Stack auf der VM mit finalem Image und eigener Konfiguration:
  Admin-Setup 200; kompletter synthetischer Scanablauf je 17/17 über HTTP und HTTPS.
  Test-CA für diesen Test gezielt vertraut, Produktionszertifikat unverändert.
  Keine Paperless-Verbindung und keine produktiven Dokumente in diesem Test.
- Produktion: HTTPS-Zertifikatsprüfung erfolgreich, Admin `/setup` 200,
  Scanner-HTTPS gesund und `auth_type:userpass` bestätigt.
- Vorhandene private Scanner-Zugangsdaten: Login, drei Jobs, Einstellungen,
  Logout und anschließende 401 erfolgreich. Kein Produktionsscan angelegt.
- Vorhandenes Paperless-Token: read-only Task-Abfrage mit API-Version 10 liefert 200.
  Keine Dokumente importiert oder geändert. Paperless bleibt auf offiziellem 3.4.0.
- Hashes und Modi von `.env`, `config/`, `secrets/` und allen vorherigen Scandateien
  vor/nach dem Update identisch. Jobs, Pepper und Zugänge erhalten.

## Rollback und noch notwendige Benutzeraktion

VM: privates `backups/dependencies-20261031/source-config.tar`, Integritätsmanifest
`state-before.json`, altes Image `nx-boss-rs:pre-dependencies-20261031`.
Zurückrollen durch Wiederherstellen der betroffenen Quell-/Builddateien und
Retaggen des alten Images; aktuelle Scans und Config nicht durch alte Daten ersetzen.
Die bestehende Homelab-Sicherung findet den Scanner weiterhin über sein Service-Label.

Auf der VM sind noch weder `admin-passkeys.json` noch `admin-auth.json` vorhanden.
Der private Einrichtungscode liegt in `config/admin-setup-token` (0600).
Der Betreiber muss ihn auf seinem eigenen Rechner lesen und auf
https://nx.maindorfer.name/setup einen eigenen Passkey registrieren.
Der private Schlüssel bleibt auf seinem Authenticator; die Einrichtung darf
nicht durch einen virtuellen Testauthenticator in Produktion ersetzt werden.
Anschließend unter `/account` einen zweiten Authenticator als Ersatz registrieren.
Siehe [Passkey-Anleitung](admin-passkeys.md).

Ein neuer physischer Scan am fi-7300NX wurde bei dieser Wartung nicht durchgeführt;
der letzte dokumentierte physische Erfolgsnachweis stammt vom 01.10.2026.
