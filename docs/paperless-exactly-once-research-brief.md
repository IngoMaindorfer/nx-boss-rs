# Forschungsauftrag: Genau-einmal-Import von nx-boss nach Paperless-ngx

## Auftrag und Grenze

Untersuche **nur lesend**, was im tatsächlich eingesetzten Paperless-ngx 3.2.1 und in nx-boss nötig wäre, damit automatische Wiederholungen nach verlorenen Antworten, Prozessabbruch und Stromausfall nicht zu mehreren Dokumenten für denselben Scan-Batch führen. Erstelle einen begründeten technischen Entwurf. **Nichts implementieren, keine Migration ausführen, keine Konfiguration ändern, keine Container neu starten und keine Testdokumente hochladen oder löschen.** Schreibe ausschließlich den Forschungsbericht nach `docs/paperless-exactly-once-analysis.md` im nx-boss-Projekt.

Lies zuerst `AGENTS.md`, `README.md`, `docs/security-review.md`, `src/paperless.rs`, `src/batch.rs` und die [Paperless-v3-Übergabe](/home/ingo/projekte/homelab/docker/nx-boss/handover-paperless-v3.md). Erhalte sämtliche vorhandenen, teils uncommitteten Arbeiten. Gib weder API-Token noch Dokumentinhalte, personenbezogene Metadaten oder vollständige produktive Task-Antworten aus.

## Ziel präzise festlegen

Formuliere vor der Codeanalyse eine überprüfbare Garantie. Ausgangspunkt:

> Für einen von nx-boss erzeugten Batch mit stabiler Batch-ID darf Paperless höchstens **ein** zugeordnetes Dokument anlegen. Nach einem angenommenen, gültigen Upload muss nx-boss auch nach Absturz und automatischer Wiederholung schließlich dieselbe Task- und Dokument-ID ermitteln können. Ein erneuter POST mit demselben Schlüssel und identischer Anfrage darf keine zweite Verarbeitung auslösen; derselbe Schlüssel mit anderem Inhalt muss eindeutig abgewiesen werden.

Prüfe ausdrücklich, welche Teile davon überhaupt formal erreichbar sind. Trenne die Anzahl der HTTP-Requests, Task-Ausführungen, gespeicherten Dokumente und nachgelagerten Effekte wie Workflows, Webhooks und KI-Aufgaben. Definiere den Geltungsbereich bei absichtlich gelöschten Dokumenten, dauerhaft ungültigen PDFs, bewusst erneut gescannten gleichen Seiten und zwei **verschiedenen** Batch-IDs mit identischem PDF. Benenne Annahmen über Datenbank, Dateisystem, Redis/Celery, Backups und Aufbewahrungsdauer. Falls nur eine eingeschränkte „effektiv einmal“-Aussage möglich ist, formuliere diese statt „genau einmal“.

## Version und Codebasis

Untersuche primär den **laufenden** Paperless-Code, nicht nur den aktuellen `dev`-Branch. Die Homelab-Image-Definition liegt unter `/home/ingo/projekte/homelab/docker/paperless/image/`; `last-build.json` nennt als Basis `ghcr.io/paperless-ngx/paperless-ngx:3.2.1@sha256:5fa76604a81df6945086e0837b14b56543d137e8ce4f311cc5d9ebe907e74e79` mit einem separaten PR-14202-Overlay für KI-Kompatibilität. Vergleiche die tatsächlich laufende Image-ID und die relevanten Quelldateien mit dieser Basis. SSH-Alias für **lesenden** Zugriff auf die VM: `ssh -F /home/ingo/.ssh/config docker`; Paperless läuft unter `/opt/paperless`. Dokumentiere Commit, Image-Digest, Dateipfad, Funktion und Zeilennummer für jede zentrale Aussage. Unterscheide beobachtetes Verhalten, Quellcodebeleg und Vermutung.

Verfolge insbesondere diese Pfade im installierten `/usr/src/paperless/src/`:

1. `documents/views.py`: `PostDocumentView.post`, Prüfung der Berechtigung, Multipart-Eingang, temporäre Datei, `consume_file.apply_async` und Rückgabe der Task-UUID. Wo liegen zwischen Dateischreiben, Queue-Auftrag und HTTP-200 nicht atomare Fenster? Wann gilt ein Upload aus Sicht des Servers als angenommen und wie haltbar ist die Datei dann?
2. `documents/tasks.py`, Task-Signale/-Status und `documents/models.py` (`PaperlessTask`): Entstehung, Persistenz, Sichtbarkeit, Berechtigungen, Aufbewahrung und Löschung von Task-Einträgen und `result_data`. Was geschieht bei Worker-Abbruch, Redis-Ausfall, erneuter Zustellung und Task-Neustart? Kann eine Task-ID oder ein dauerhaftes Ergebnis nach Ablauf der Task-Historie wiedergefunden werden?
3. `documents/consumer.py`, Dokumentmodell und Dateispeicherung: Zeitpunkt von Duplikatprüfung, Hash-Bildung, Pre-Consume-Skripten, Datenbanktransaktion und finalen Dateien. Gibt es eine **Datenbank-Unique-Constraint** für einen stabilen Client-Schlüssel? Sind zwei parallele POSTs oder Worker durch bloßes Prüfen vor dem Einfügen gegen ein Rennen geschützt? Welche Seiteneffekte können vor einem Rollback schon ausgelöst sein?
4. API v10 (`documents/serialisers.py`, Task-/Dokument-Views und Filter): Welche Abfrage würde einen Batch auch nach Task-Bereinigung zuverlässig auf dieselbe Dokument-ID abbilden? Prüfe dabei Rechte und Sichtbarkeit für verschiedene Benutzer. Die momentane nx-boss-Suche nach `<batch-id>.pdf` ist ein Wiederfinde-Hinweis, **kein** atomarer Idempotenzbeleg.
5. Globale Duplikaterkennung: Was garantiert `PAPERLESS_CONSUMER_DELETE_DUPLICATES=true` tatsächlich bei gleichen Bytes, parallelen Imports, veränderten Dateien durch Pre-Consume-Skripte und verschiedenen Batch-IDs? Beachte, dass die Einstellung alle Paperless-Importe betrifft und ein abgewiesener Duplikat-Task als Fehler endet.

Die [offizielle Upload-API](https://docs.paperless-ngx.com/api/#posting-documents) beschreibt eine asynchrone Task-UUID und die spätere Task-Abfrage; sie dokumentiert keinen Idempotency-Key. Die [Konfiguration](https://docs.paperless-ngx.com/configuration/#paperless_consumer_delete_duplicates) beschreibt die globale Prüfsummen-Duplikaterkennung. Verifiziere beides im **installierten** Code und halte Abweichungen vom Upstream fest. Die [Hinweise zu Pre-Consume-Skripten](https://docs.paperless-ngx.com/advanced_usage/#pre-consume-script) sind für Hash- und Wiederholungsfragen relevant.

## Ausfall- und Nebenläufigkeitsanalyse

Zeichne die aktuelle Abfolge vom dauerhaften nx-boss-Sendeversuch bis zur lokal gespeicherten Paperless-Dokument-ID. Lege an **jeder** Grenze vor/nach folgenden Ereignissen einen gedanklichen Crash ein: Annahme des HTTP-Bodys; temporäre Datei; Datenbankeintrag; Broker-Publish; HTTP-Antwort; Worker-Start; Duplikatprüfung; Dokument-DB-Commit; Datei-Rename/-Sync; Workflow-/KI-/Webhook-Start; Task-Ergebnis; Antwortverlust beim Client; Task-Bereinigung. Ergänze gleichzeitige identische POSTs, Neustart beider Container und Ausfall von Datenbank, Redis oder Datenträger. Für jeden Fall: Was ist nach Neustart dauerhaft vorhanden, was sieht nx-boss, und kann eine automatische Wiederholung ein zweites Dokument oder einen verlorenen Import erzeugen?

Prüfe dabei besonders die Grenze zwischen PostgreSQL und Redis/Celery sowie zwischen Datenbank und Dateisystem. Ein `transaction.atomic()` für die Datenbank allein macht diese drei Systeme nicht zu einer Transaktion. Prüfe, ob eine persistente Empfangszeile plus Outbox/Recovery-Worker nötig wäre, und ob bereits vorhandene Paperless-Strukturen dafür nutzbar sind.

## Lösungswege bewerten, nicht umsetzen

Vergleiche mindestens diese Kandidaten anhand der oben definierten Garantie:

- Idempotency-Key als API-Header oder Form-Feld, serverseitig mit `(Benutzer/Mandant, Schlüssel)` eindeutig in der Datenbank gespeichert; dazu Hash der **gesamten relevanten Anfrage** und dauerhafte Zustände wie `accepted`, `queued`, `processing`, `succeeded`, `failed`.
- Eindeutiges, dauerhaftes Batch-ID-Feld am Dokument und eine sichere Zuordnung schon **vor** dem asynchronen Konsum; prüfe, ob Paperless-Custom-Fields dafür überhaupt atomar und eindeutig genug sind.
- Content-Hash-Duplikaterkennung, deterministische Celery-Task-ID, Suche nach Dateiname oder Custom Field sowie rein clientseitige Wiederholung. Erkläre für jeden Weg die konkreten Lücken und möglichen falschen Treffer.

Für einen empfohlenen Weg beschreibe nur das **vorgeschlagene** API-Verhalten: Erst-POST, gleichzeitige Wiederholung, gleiche ID mit anderem Payload (z. B. 409), Anfrage während laufender Verarbeitung, dauerhaft fehlgeschlagener Import, gelöschtes Dokument, Schlüssel-Aufbewahrung und Wiederherstellung nach Backup. Kläre, ob Wiederholungen dieselbe Task-ID zurückgeben müssen oder ein separater Status-Endpunkt besser ist. Liste die voraussichtlich nötigen Änderungen **getrennt** für Paperless und nx-boss sowie Migration, Rollout und Rückwärtskompatibilität. Keine Patches anfertigen.

## Gewünschter Bericht und Abnahme

Der Bericht soll enthalten:

1. Eine knappe Definition der erreichbaren Garantie mit klaren Grenzen.
2. Ein Sequenzdiagramm des Ist-Zustands und eine Tabelle der Crash- und Race-Fälle mit Codebelegen.
3. Eine Vergleichstabelle der Lösungswege und eine begründete Empfehlung, einschließlich der erforderlichen Datenbank-Constraints, Aufbewahrung und möglichen Nebenwirkungen für andere Paperless-Importe.
4. Einen **späteren** TDD-/Fault-Injection-Plan: parallele identische Requests, Antwortverlust nach Annahme, Kill an jeder Commit-Grenze, Redis-/DB-Neustart, Task-Bereinigung, Payload-Konflikt, Pre-Consume-Änderung, Dateiverlust, Wiederherstellung aus Backup und dokumentierte Grenzen für Webhooks/KI. Nur Tests planen, jetzt keine ausführen, die Daten verändern.
5. Eine Liste offener Fragen, die nur durch einen isolierten Testaufbau oder eine Produktentscheidung beantwortet werden können. Gib für jede Frage an, welche Beobachtung nötig ist und wie sie ohne Produktivdaten zu gewinnen wäre.

Abnahme der Forschungsarbeit: Alle Aussagen über Garantien haben einen Beleg oder eine ausdrücklich genannte Annahme; der Entwurf erklärt jedes identifizierte Absturzfenster. Produktionsdaten, Tokens, laufende Dienste und Produktcode bleiben unverändert.
