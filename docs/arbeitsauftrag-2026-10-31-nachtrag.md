# Arbeitsauftrag nx-boss-rs, Nachtrag vom 31.10.2026

**Auftraggeber:** Betreiber (Ingo)
**Grundlage:** [review-2026-10-31.md](review-2026-10-31.md), Abschnitt „Nachprüfung“ mit N1 bis N4 und F2, dazu B07 und B24. Der [Arbeitsauftrag vom selben Tag](arbeitsauftrag-2026-10-31.md) ist abgeschlossen, seine Definition of Done gilt weiter.

## Ziel

1. Stand 793db1c läuft auf der VM.
2. N1, N2 und N3 sind behoben.
3. Die Protokoll-Probes sind entfernt (B24).
4. „Diesem Gerät vertrauen“ ist eingebaut und ausgerollt (F2).
5. Ein Konzept für B07 liegt zur Freigabe vor.

## Entscheidungen, die feststehen

- **F2:** Feste Frist von 30 Tagen, Details siehe Phase 3. B20 bleibt abgelehnt.
- **B24:** Entfernen. NFC funktioniert im Scanner nicht, die Messung ist beendet.
- **B07:** Unterbrechungen des Scanners durch das Backup sind nicht akzeptabel, ein laufender Scan darf nie unterbrochen werden. Dein Vorschlag im Review reicht deshalb nicht aus, weil er nx-boss weiter stoppt. Gefordert ist in diesem Auftrag ein Konzept, keine Umsetzung.

## Rahmen

- `AGENTS.md` gilt: zuerst ein fehlschlagender Test, keine Secrets in Ausgaben oder Doku, `.env`, `secrets/` und `scans/` sowie Inhalte in `config/` nicht lesen.
- **Freigegeben:** Commit, Push und Merge nach `main` ohne Force-Push nach grüner Branch-CI. Dazu Deployment nach `/opt/nx-boss-rs` und Neustart der Dienste `nx-boss-rs` und `caddy`.
- **Nicht freigegeben:** Änderungen an `.env`, `config/` oder am laufenden Backup, ein Export-Lauf von Hand, Neustart von Paperless oder anderen Stacks, Rotation der Scanner-Zugangsdaten.
- Im Repo `homelab` nichts ändern und nichts committen.
- Der Arbeitsbaum enthält die Betreiber-Änderungen an `docs/review-2026-10-31.md` und diese Datei. Beide gehören in den ersten Commit.

## Regeln für jedes Deployment

1. Quelle ist ausschließlich ein Commit auf `main` mit grüner Main-CI. Übertragen per `git archive`, Build in einem getrennten Verzeichnis.
2. **Nicht während eines Scans:** Die Logs dürfen in den letzten 15 Minuten keine `/NmWebService/batch`- oder `/NmWebService/image`-Anfragen zeigen.
3. **Nicht während eines Exports:** `ps -eo args | grep "[r]estic-export"` muss leer sein, und bis zum nächsten Lauf müssen mindestens 15 Minuten liegen. Die Zeiten stehen in `homelab/backup/restic/*.conf`. `pgrep -f` taugt über SSH nicht, es findet die eigene Kommandozeile.
4. Vor dem Wechsel das laufende Image datiert taggen (`nx-boss-rs:pre-<thema>-YYYYMMDD`).
5. App geordnet stoppen, dann Quellstand und private Daten als Tar nach `backups/pre-<thema>-YYYYMMDD/` mit Modus 0700 und 0600 sichern. Der Inhalt wird nicht angesehen.
6. Dateien einspielen ohne `--delete` und ohne Überschreiben der Verzeichnisrechte von `/opt/nx-boss-rs`. Entfallene Dateien gezielt entfernen.
7. Das neue Image auf `nx-boss-rs:local` taggen und starten mit `docker compose up -d --no-build --force-recreate --wait --wait-timeout 180 nx-boss-rs caddy`. `NX_BOSS_IMAGE` nicht verwenden, siehe N2.
8. Prüfen: beide Container healthy, laufendes Image ist das neue, `:20447/health` 200, Admin-HTTPS antwortet, HSTS und CSP gesetzt, keine neuen `ERROR` im Log, Footer zeigt den Commit.
9. Schlägt Schritt 8 fehl: sofort zurückrollen und berichten. Compose und Caddyfile aus dem Quell-Tar zurückholen, das `pre-`Image auf `nx-boss-rs:local` taggen, denselben `up`-Befehl ausführen. `scans/` und `config/` nicht zurückspielen.

## Sofort anhalten und nachfragen, wenn

- eine Sicherheitsabfrage oder Berechtigungsprüfung einen Schritt ablehnt. Nicht umgehen und nicht in Teilschritte zerlegen.
- ein Secret-Scan einen echten Treffer liefert oder eine private Datei in der Commit-Liste steht.
- nach einem Rollback der alte Stand nicht healthy ist.
- die Entfernung der Probes das normale `userpass`-Verhalten am Scanner berühren würde.

## Phase 1: Stand ausrollen

793db1c ist geprüft, aber noch nicht deployt. Das Image `nx-boss-rs:review-793db1c` und der Quellstand `/home/ingo/nx-review-build` liegen auf der VM und dürfen verwendet werden, wenn `main` weiter auf diesem Commit steht oder nur Dokumente abweichen. Tag-Thema: `pre-review-20261031`.

**Abnahme:** Regeln 1 bis 8 erfüllt. Die Antworten und die Übergabe nennen Uhrzeit und Image-ID.

## Phase 2: Findings und Probes

**N1:** `build_sidecar` nimmt Datum und Titel aus derselben lokalen Zeitzone wie der API-Pfad. Die Logik nur einmal halten. Test mit 22:30 UTC im Sommer und 23:30 UTC im Winter.

**N2:** [deployment-review-2026-10-31.md](deployment-review-2026-10-31.md) auf das Verfahren oben umstellen. Überholte Punkte aus N4 dort korrigieren. `docs/operations.md` prüfen.

**N3:**

- Serverseitige Zustandsprüfung, `complete` und `discard` antworten sonst mit 409:

  | Aktion | Erlaubt für |
  |---|---|
  | `complete` | offenen, nicht beschädigten Batch ohne Aktivität seit mindestens einer Stunde |
  | `discard` | offenen Batch ohne Aktivität seit mindestens einer Stunde, oder beschädigten Batch |

- Aktivität ist der jüngste Zeitpunkt aus `created_at` und den `received_at` der Seiten. Bei beschädigten Batches gilt das Verzeichnis-`mtime`.
- Die Oberfläche zeigt die Buttons nur unter denselben Bedingungen. Beide Stellen nutzen dieselbe Funktion.
- Abgeschlossene und zugestellte Batches lassen sich nicht verwerfen. Das Löschen bleibt der Retention überlassen.
- Der Step-up wird erst verbraucht, wenn PDF-Slot und Zustandsprüfung bestanden sind, und zwar in demselben gesperrten Abschnitt. Ein 503 oder 409 kostet keine Bestätigung.
- Beim Start werden liegengebliebene `.discarded-<id>`-Verzeichnisse mit gültiger ID entfernt. Fehler dabei werden gemeldet, verhindern aber den Start nicht.

**B24:**

- Entfernen: `NX_BOSS_SCANNER_AUTH_PROBE`, `NX_BOSS_SCANNER_AUTH_EXPERIMENT`, `scanner_auth_experiment`, `probe_matches`, die Probe- und Experimentfunktionen in `routes/scanner.rs` samt Aufrufen in `routes/batch.rs` und `routes/image.rs`, die Zweige in `routes/mod.rs` und `main.rs`, beide Zeilen in `docker-compose.yml`, die zugehörigen Tests und die Teile von `scripts/test_logging.py`.
- Der normale `userpass`-Ablauf bleibt unverändert. Die Secret-Redaktion in `test_logging.py` bleibt mit echten Pfaden abgedeckt.
- `scripts/probe_sides.py` und `scripts/rev.py` einzeln prüfen und nur entfernen, wenn sie allein der Messung dienen.
- `config/scanner-auth-probe-password` trotz des Namens nicht anfassen. Die Datei kann der aktive Scanner-Zugang sein, siehe B12.
- `docs/scanner-auth-tls-research.md`: festhalten, dass NFC im Scanner nicht funktioniert und die Messung beendet ist. Die Wiederaufnahme-Anleitung dazu entfällt.

**Abnahme:** `rg -i 'probe|auth_experiment' src docker-compose.yml` findet nur noch Fachfremdes, das in der Antwort begründet ist. Volle Definition of Done, Testablauf `userpass` im Browser und in `test_shutdown.py` grün.

## Phase 3: Diesem Gerät vertrauen (F2)

**Verhalten:**

- Auf `/login` steht das Häkchen „Diesem Gerät 30 Tage vertrauen“ mit optionalem Gerätenamen, standardmäßig aus, in Deutsch und Englisch. Das Passkey-Setup bietet es nicht an.
- Nach erfolgreicher Passkey-Anmeldung mit Häkchen setzt der Server das Cookie `__Host-nx-trust` mit `Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/` und 30 Tagen `Max-Age`.
- Bei einem `GET` ohne gültige Sitzung und mit gültigem Geräte-Cookie entsteht eine normale Sitzung, ohne Passkey-Dialog. Andere Methoden antworten wie bisher mit 401.
- Die Frist läuft fest ab der Passkey-Anmeldung und verlängert sich nicht. Sie ist eine Konstante, keine Einstellung.
- So wiederhergestellte Sitzungen haben keinen Step-up. Alle bisher geschützten Aktionen verlangen weiter den Passkey.

**Speicherung:**

- 256 Bit Zufall aus der vorhandenen Quelle. Auf der Platte liegt nur ein Hash, Vergleich mit `crate::security::constant_time_eq`. Eine direkte `sha2`-Abhängigkeit ist erlaubt.
- Datei `config/admin-trusted-devices.json`: privat, atomar geschrieben, gleiche Prüfung wie bei `admin-passkeys.json`. Inhalt: Name, Hash, Erstellt, Läuft ab, Zuletzt genutzt. Zeiten sind absolute UTC-Zeiten, keine `Instant`.
- Höchstens 8 Geräte, beim neunten fällt das älteste heraus.
- „Zuletzt genutzt“ höchstens einmal täglich schreiben.
- Eine fehlende oder beschädigte Datei stoppt den Dienst nicht. Dann gilt kein Gerät als vertraut, der Passkey-Login funktioniert weiter, und es entsteht ein redigiertes Warnereignis.

**Widerruf:**

- `/account` listet die Geräte mit Name, Erstellt, Läuft ab, Zuletzt genutzt und Markierung des aktuellen Geräts. Entfernen und „Alle entfernen“ verlangen Step-up.
- Das Entfernen eines Passkeys widerruft alle vertrauten Geräte.
- Abmelden widerruft das aktuelle Gerät serverseitig und löscht das Cookie.
- Abgelaufene oder unbekannte Cookies werden gelöscht.
- Der Token erscheint nie in Logs, Ereignissen oder Fehlertexten.

**Doku:** `admin-passkeys.md`, `security-review.md` und `SECURITY.md` nennen das Restrisiko: Wer ein vertrautes, entsperrtes Gerät hat, sieht Scans und kann Jobs und Fristen ändern. Empfehlung dort: nur eigene Geräte mit Sperrbildschirm.

**Abnahme, je mit Test:**

- Login ohne Häkchen setzt kein Cookie.
- Mit Häkchen ist der Zugriff nach Sitzungsende und nach Neustart des Binaries ohne Passkey möglich.
- Am 31. Tag ist der Zugriff abgelehnt und das Cookie gelöscht.
- Step-up-Aktionen bleiben mit vertrauter Sitzung gesperrt.
- Widerruf über `/account`, Passkey-Entfernung und Abmelden wirken jeweils sofort.
- Die Datei enthält den Klartext-Token nicht und hat Modus 0600.
- Beschädigte Datei: Dienst startet, Passkey-Login funktioniert.
- Falsche Origin bei den neuen POST-Routen ergibt 403, fehlende Sitzung 401.
- Die Routentabelle aus `protected_mutations_require_session_and_same_origin` ist erweitert.
- Browser-Test mit virtuellem Authenticator, Sichtprüfung auf Handybreite und im Dark Mode, Deutsch und Englisch.

## Phase 4: Zweites Deployment

Stand aus Phase 2 und 3 nach den Regeln oben ausrollen. Tag-Thema: `pre-trust-YYYYMMDD`. Der Rollback auf den Stand davor ignoriert die neue Datei, vertraute Geräte fallen dann auf den Passkey zurück. Das in der Rollback-Doku nennen.

## Phase 5: Konzept B07

Datei `docs/backup-konzept-nx-boss.md`. Keine Umsetzung, kein Export von Hand, keine Änderung am Skript.

**Anforderungen des Betreibers:**

1. Ein laufender Scan wird nie unterbrochen.
2. Der Export stoppt nx-boss nicht. Das ist das Zielbild. Eine Abweichung davon muss das Konzept ausdrücklich begründen, mit Dauer, Häufigkeit und Auswirkung auf den Scanner.
3. Scanner-Sitzungen überleben einen Export.
4. Der Stand bleibt wiederherstellbar. Eine stille Lücke im Backup ist nicht akzeptabel: Ein ausgelassener oder eingeschränkter Lauf muss in `restic-health` sichtbar werden, und die Wartezeit auf Scans braucht eine Obergrenze.

**Zu klären:**

- **Ist-Messung, nur lesend:** Wie lange ist nx-boss je Export offline? Quellen sind Container-Startzeiten, `shutdown`-Ereignisse und Journald. Wie häufig trafen Exporte schon Scan-Zeiten?
- **Was der Stopp heute wirklich absichert,** getrennt nach Seiten, `metadata.json`, Zustellbeleg, Konfiguration und Archiv.
- **Bewertung dieser Varianten:**
  - Export im laufenden Betrieb, mit Regeln für offene Batches und einer Prüfung des Manifests.
  - Hardlink-Snapshot von `scans/` unter kurzer interner Sperre. Atomares Ersetzen lässt die verlinkten Dateien unverändert, zu prüfen sind das In-Place-Verhalten beim Anhängen und die Grenzen des Dateisystems.
  - Export durch nx-boss selbst unter seinen Sperren, ausgelöst von einem nur lokal erreichbaren Weg.
  - Snapshot auf Proxmox-Ebene. Die VM hat ext4 ohne LVM, zu prüfen sind Storage und Auswirkung von `fsfreeze` auf laufende Uploads.
  - Warten auf offene Batches als reine Rückfallebene.
- **Persistente Scanner-Sitzungen:** Nutzen, Risiko und Hash-Speicherung. Das hilft auch bei Deployments, ersetzt aber allein Anforderung 1 nicht.
- **Kopplung an Paperless:** Während des Exports bleiben Webserver und Broker gestoppt. Bitte belegen, dass das Scans nicht stört und die Zustellung danach sauber nachläuft. Der Zeitversatz zwischen Paperless-Dump und Zustellbelegen muss eine dokumentierte Wiederherstellungsgrenze mit Abgleich bekommen.
- **Restore-Test:** Wie sich die empfohlene Variante isoliert beweisen lässt, einschließlich eines Exports mitten im Scan.

**Ergebnis:** eine klare Empfehlung mit den nötigen Änderungen an Skript und App, Aufwand, Risiken und Rollback, dazu die offenen Fragen an den Betreiber. Nicht empfohlene Varianten nur mit kurzer Begründung.

**Abnahme:** Antwort unter B07 im Review verweist auf das Konzept. Die Umsetzung gibt der Betreiber frei.

## Nicht Teil dieses Auftrags

- **B20:** bleibt abgelehnt.
- **N4 und B12:** Rotation, Ersatz-Passkey und Entfernen der alten Passwortdatei macht der Betreiber.

## Definition of Done

- Definition of Done aus dem ersten Auftrag für jeden Merge.
- Im Review sind die Antwortfelder zu N1 bis N3, F2, B07 und B24 ausgefüllt, mit Test-Name und Commit. Befundtexte bleiben unverändert.
- Neuer Eintrag oben in `AGENTS.md`. Der Eintrag „noch nicht deployed“ wird korrigiert.
- Ergebnisse werden so berichtet, wie sie sind, auch fehlgeschlagene oder übersprungene Prüfungen.

## Liefergegenstand am Ende

Eine kurze Übergabe an den Betreiber mit:

1. dem Stand je Phase
2. Uhrzeit, Commit, Image-ID und Rollback-Tag beider Deployments
3. dieser Liste, die nur der Betreiber erledigen kann:
   - Häkchen mit echtem Passkey testen, Gerät auf `/account` widerrufen
   - Ersatz-Passkey anlegen
   - Scanner-Zugangsdaten rotieren (B12), danach alte Passwortdatei entfernen
   - physischer Testscan bis Paperless 3.4.0
   - physischer Test: Verhalten des fi-7300NX nach 401 und Neustart mitten im Scan (B02)
   - Entscheidung zum B07-Konzept
