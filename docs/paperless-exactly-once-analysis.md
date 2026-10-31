# Genau-einmal-Import von nx-boss nach Paperless: Analyse und Entwurf

Stand: 30.09.2026. **Forschungsbericht, keine Implementierung.**

## 1. Ergebnis und überprüfbare Garantie

**Der laufende Stand bietet Best-Effort-Wiederfinden und globale Inhalts-Duplikatprüfung, aber weder höchstens ein Dokument pro Batch noch garantierten Abschluss nach Absturz.** Für die stärkere Zusage sind Änderungen in Paperless nötig. Ein ausschließlich clientseitiger nx-boss-Fix kann die fehlende atomare Entscheidung des Servers nicht ersetzen. Besonders kritisch: getrennte PostgreSQL-/Redis-Schreibvorgänge, Prüfsummenprüfung ohne Unique-Constraint, Dateisystemänderungen innerhalb einer nur für die Datenbank atomaren Transaktion und nicht dauerhaft an den Dokument-Commit gekoppelte Ergebnisse. Belege: C1–C9, N1–N3 unten.

Vor der Codeanalyse verwendeter Prüfmaßstab, hier präzisiert:

- Identität ist `K = (stabiler Integrations-Namensraum, Batch-ID)`, nicht Dateiname, Tokenwert oder PDF-Prüfsumme.
- `F` bezeichnet einen versionierten Fingerabdruck der vollständigen relevanten Anfrage.
- **Safety:** Innerhalb einer fortbestehenden, konsistenten Serverhistorie wird für K höchstens eine Dokumentidentität angelegt. Alle bestätigten Wiederholungen von (K,F) beziehen sich darauf. Ein anderer Fingerabdruck für K wird abgewiesen.
- **Wiederfindbarkeit:** Der Server bewahrt die Operation und ihr Ergebnis unabhängig von flüchtigen Celery-Ergebnissen und aufräumbarer Task-Historie auf.
- **Bedingte Liveness:** Nach dauerhafter Annahme wird ein zulässiger, erfolgreich verarbeitbarer Einzeldokument-Import schließlich unter derselben Operations-ID mit derselben Dokument-ID bestätigt, sofern Dienste und Datenträger wieder verfügbar werden, Recovery fair wiederholt wird und Daten sowie Rechte erhalten bleiben. Dauerhaft ungültige oder durch Importregeln abgewiesene Eingaben erhalten stattdessen ein dauerhaftes Fehlerergebnis.
- Ein wiederholter POST darf **keine neue logische Importoperation** erzeugen. Dass nach einem Crash überhaupt keine erneute physische Ausführung stattfinden darf, wäre mit zuverlässigem automatischem Abschluss unvereinbar. Recovery muss bereits begonnene Rechenarbeit wiederholen dürfen.

Diese Kombination heißt hier **effektiv einmal bezüglich der Dokumentidentität**, nicht universell „genau einmal“:

| Ebene | Erreichbare Zusage des vorgeschlagenen Designs |
| --- | --- |
| HTTP-Anfragen | Mehrere Requests sind erlaubt und unvermeidbar, wenn Antworten verloren gehen. |
| Logische Importoperation | Eine je K durch dauerhafte DB-Eindeutigkeit. |
| Celery-Nachrichten und Task-Ausführungen | Können mehrfach vorkommen; jeder Versuch muss gegen dieselbe Operation geprüft werden. |
| Dokumentidentität und erfolgreiche DB-Finalisierung | Höchstens eine je K; bei erfüllten Liveness-Annahmen schließlich genau eine. |
| OCR/Parsing und Dateivorbereitung | Können nach Absturz mehrfach ausgeführt werden. |
| Workflow-DB-Änderungen | Einmalige Finalisierung möglich, wenn in dieselbe DB-Transaktion integriert. |
| Webhook, Mail, externe KI-/OCR-Anfrage | Keine allgemeine Exactly-once-Garantie ohne kooperierenden Empfänger mit dauerhaftem Idempotenzprotokoll. Auch doppelte Kosten sind möglich. |

**Geltungsbereich:** Absichtlich gelöschte Dokumente werden bei Wiederholung desselben K nicht neu angelegt; ein Tombstone beantwortet den Status. Ein bewusst neuer Scan erhält eine neue Batch-ID, auch bei gleichen Seiten. Zwei unterschiedliche K mit identischem PDF sind unterschiedliche Aufträge: die globale Inhalts-Duplikatregel bleibt eine gesonderte Produktentscheidung. Ungültige PDFs, Workflow-Abbruch und Inhalts-Duplikatablehnung sind terminale Ergebnisse, kein Versprechen auf eine Dokument-ID. Automatisches Splitten oder Zusammenführen muss für den zunächst empfohlenen Einzeldokument-Modus ausgeschlossen sein. Die bestehende Barcode-Pipeline kann zusätzliche Consume-Tasks anlegen (C12).

**Annahmen:** PostgreSQL-Constraints und bestätigte Commits bleiben erhalten; fsync einschließlich Verzeichnissen und Storage-Cache-Barrieren wird vom Datenträger/Hypervisor eingehalten; Eingangsdateien und Empfangsregister werden gemeinsam gesichert; Schlüssel werden nicht wiederverwendet; kein zweiter Importweg umgeht die Zuordnung. Bei dauerhaftem Datenverlust, willkürlichem Rollback oder nicht kooperierenden externen Systemen gilt keine unbegrenzte Liveness-/Exactly-once-Zusage. Diese Grenzen werden in Abschnitt 8 konkretisiert.

## 2. Untersuchter Stand und Nachweisart

**Nur lesend untersucht:** lokale Projektdateien, laufende Container-Quelldateien, Docker-Image-Metadaten und vorhandene Image-Layer; PostgreSQL ausschließlich Schema/Settings in einer READ-ONLY-Transaktion; Redis ausschließlich CONFIG GET für zwei Persistenzeinstellungen. Keine Dokument-/Task-Inhalte, Tokens oder vollständigen produktiven API-Antworten ausgegeben. Keine API-Uploads, Löschungen, Migrationen, Konfigurationsänderungen, Neustarts oder Fault-Injection-Tests. Einziger angelegter Inhalt ist dieser Bericht.

Kennzeichnung: **B** = in dieser Untersuchung beobachteter Zustand; **C/N** = statischer Quellcodebeleg; **A** = explizite Annahme oder daraus abgeleiteter Crash-/Race-Fall; **E** = noch nicht implementierter Entwurf. Ein theoretischer Ablauf in einer Tabelle ist kein behaupteter reproduzierter Produktionsfehler.

### Versionen und Vergleich mit der Basis

- Laufendes Paperless: `homelab/paperless-ngx:3.2.1-pr14202`.
- Laufende Image-ID: `sha256:e51621abba79dd1d1d4b59f18b3bb3b57a5027c2b9bcb40fb4f2a830aa59d952`.
- Basis: `ghcr.io/paperless-ngx/paperless-ngx:3.2.1@sha256:5fa76604a81df6945086e0837b14b56543d137e8ce4f311cc5d9ebe907e74e79`.
- Basis-Revision laut OCI-Label: `7575d6078227ebdb4cf443f263d53ebc7575aa37`.
- KI-Overlay laut Homelab-Provenienz: PR #14202, Merge `04297fd02c3d3b6a3f13e210a49e3f4c3c4009b3`.
- Celery 5.6.3, Kombu 5.6.2, Django 5.2.16 laut installierten Paketmetadaten.
- nx-boss-HEAD: `58fe23cb1a006979981af618ab58994a9d9be499`, **mit umfangreichen uncommitteten Änderungen**. HEAD allein beschreibt den untersuchten Code nicht.
- Laufendes nx-boss-Image: `sha256:cd3aed8e97c87af2dd93517bda8cc7bb5e550d4042090b2a6ac50288979ab659`, healthy. Lokale Scanner-Quelldateien unten stimmen bytegleich mit `/opt/nx-boss-rs/src/` auf der VM überein. Dies ist ein Quellvergleich, kein reproduzierbarer Nachweis der Binärherkunft.

Die ersten 15 RootFS-Layer des vorhandenen Paperless-Imagearchivs stimmen mit der Basis-Layerliste überein. Die relevanten Dateien wurden daraus ausschließlich im Lesestrom gehasht und mit dem laufenden Container verglichen. Ein erster Vergleich konnte die komprimierten Blob-Namen nicht zuordnen; er wurde verworfen. Der korrigierte Vergleich mit dem Archivmanifest ergab:

| Datei unter /usr/src/paperless/src/ | SHA-256 im laufenden Container | Basisvergleich |
| --- | --- | --- |
| documents/views.py | 6000bf1814bd87fab4ee8cac1fc0664aeb491acb1e73c0a47c8b424fe201d72b | identisch |
| documents/tasks.py | e916133fdf8b447a70194c2768d10d0cd1880089d0af962bc65037ca24d194ee | identisch |
| documents/models.py | e5dfbdac82ea52a40b0b8665561b4fc8cc4890aa7ea2adc63c94f48b6d8b01d2 | identisch |
| documents/consumer.py | 2f50d8ad41e06b6f52a57cbf0cdec0e8ddc25b3b47f752569394c72f7c250a38 | identisch |
| documents/signals/handlers.py | 4e22cc7d30f1c0c32243e0ea43fa1a97e54d09ebfbcbd99ae4f13c17ef7f3987 | identisch |
| documents/serialisers.py | 7ad1bee65cd65e78035ce255409c2e952395f8c53cbef3d2b4db655a9d968fdb | identisch |
| documents/filters.py | 009c82a63001c2b49c11d76d1c9850fb105353969ec6f3f9c03c2d4fc796a362 | identisch |
| documents/utils.py | 83d2a395fad2424ec01416e28069471902d479851164f8a90e6767dcdd24cdc0 | identisch |
| documents/permissions.py | bb2af1efd059d69f489dea57fb579cf78967e607455820cedc2ea21226475065 | identisch |
| documents/apps.py | ad081a1dbd26656a0a29104475f088a8cb24c6b28011ad7b47b15787620b1928 | identisch |
| paperless/celery.py | 5c346889b13ff3896d471b3c50a49ea380801e367bb38c73dc64bc397756e8db | identisch |
| paperless/settings/__init__.py | d99c82fb9534bbba04fad81b94972ad94900269b3c1518e1dec829ebc3cbde77 | erwartetes KI-Overlay; Basis cd93827046e342761e3ab9f418487f15c38b15125df30514e845c0d5a07972a8 |

Scanner-Fingerabdrücke: `src/paperless.rs` = `c1df944662ce662527cbeaffc38b18cee91e2e7bcfc51810f094da06b4e331ad`; `src/batch.rs` = `8df2381b46bdff48ea4328211f19d84b26963950641f7ad1e153590e6899562e`; `src/atomic_file.rs` = `a9a6fb56f18818f883903474e6eb74fdafda4fbd869d4f18710b7c607386b21c`.

**Betriebsbeobachtungen B1:** Upload-Scratch ist ein persistenter Bind-Mount auf `/tmp/paperless`. Laufende Umgebung enthält `PAPERLESS_CONSUMER_DELETE_DUPLICATES=true`; keine Pre-/Post-Consume-Script-Variable wurde in der ausgewählten Containerumgebung gefunden. Redis meldet `appendonly=yes`, `appendfsync=everysec`. PostgreSQL meldet `fsync=on`, `synchronous_commit=on`, `full_page_writes=on`. Das beweist keine konkrete Haltbarkeit der darunterliegenden Hardware und keine gemeinsame Transaktion der drei Systeme.

**Schema-Beobachtung B2:** `pg_indexes` bestätigt: checksum/archive_checksum haben normale, nicht eindeutige Indizes. Task-ID ist eindeutig. Custom Fields haben nur Eindeutigkeit auf `(document_id, field_id)`. Eindeutige Dateipfade, ASN und Versionsnummern bilden keinen Batch-Schlüssel ab.

Die frühere [Scanner-v3-Übergabe](/home/ingo/projekte/homelab/docker/nx-boss/handover-paperless-v3.md) beschreibt einen inzwischen behobenen Parserstand. Der aktuell gelesene nx-boss-Code behandelt v10-Statuswerte und genau eine positive Dokument-ID bereits korrekt. Die hier untersuchten Zustellungsfenster bleiben davon unabhängig.

### Belegregister

Paperless-Pfade C1–C12 beziehen sich auf **/usr/src/paperless/src/** im genannten Container. Die Commit-Links dienen der Navigation; maßgeblich sind gelesener installierter Code und Fingerabdrücke. Hilfsdateien ohne Hashzeile wurden direkt installiert gelesen, nicht separat gegen die Basis gehasht.

| Beleg | Datei, Funktion und Zeilen |
| --- | --- |
| C1 | [documents/views.py, PostDocumentView.post](https://github.com/paperless-ngx/paperless-ngx/blob/7575d6078227ebdb4cf443f263d53ebc7575aa37/src/documents/views.py#L3443-L3512): 3443–3512; documents/serialisers.py, PostDocumentSerializer/validate_document: 2263–2353. |
| C2 | [documents/signals/handlers.py](https://github.com/paperless-ngx/paperless-ngx/blob/7575d6078227ebdb4cf443f263d53ebc7575aa37/src/documents/signals/handlers.py#L1075-L1388): _extract_input_data 1075–1111; before_task_publish_handler 1158–1206; task_prerun_handler 1209–1227; task_postrun_handler 1230–1288; task_failure_handler 1291–1353; task_revoked_handler 1356–1388. |
| C3 | [documents/models.py](https://github.com/paperless-ngx/paperless-ngx/blob/7575d6078227ebdb4cf443f263d53ebc7575aa37/src/documents/models.py#L767-L929): PaperlessTask 767–929, related_document_ids 922–929; Document 217–304; CustomFieldInstance.Meta 1293–1307. |
| C4 | [documents/tasks.py, consume_file](https://github.com/paperless-ngx/paperless-ngx/blob/7575d6078227ebdb4cf443f263d53ebc7575aa37/src/documents/tasks.py#L181-L278): 181–278. |
| C5 | [documents/consumer.py](https://github.com/paperless-ngx/paperless-ngx/blob/7575d6078227ebdb4cf443f263d53ebc7575aa37/src/documents/consumer.py#L989-L1077): ConsumerPreflightPlugin.pre_check_duplicate 1003–1054; run 1065–1077; documents/utils.py, compute_checksum 189 ff. |
| C6 | documents/consumer.py, run_pre_consume_script 290–328; ConsumerPlugin.run 401–808; _store 839–899; apply_overrides 901–962; _write 964–975. [Commit-Ansicht](https://github.com/paperless-ngx/paperless-ngx/blob/7575d6078227ebdb4cf443f263d53ebc7575aa37/src/documents/consumer.py#L401-L808). |
| C7 | documents/apps.py 24–35; documents/signals/handlers.py: add_to_index 795–803, run_workflows_added 806–819, run_workflows/AI-Zweig 989–1009, LLM-Index 1420–1430, update_filename_and_move_files 435 ff., Moves 593/603 und Rückversuch 625/629; documents/workflows/actions.py, execute_webhook_action 189–268. |
| C8 | paperless/settings/__init__.py 681–724 und 824; paperless/celery.py 29–38; installierte Library unter /usr/local/lib/python3.14/site-packages/: celery/app/defaults.py 265–266, 300, 332–334; celery/worker/request.py on_accepted 515–523, on_failure 610–628; kombu/transport/redis.py 644. |
| C9 | documents/views.py, TasksViewSet 4436–4557, get_queryset 4498–4524; documents/serialisers.py, TaskSerializerV10 2617–2659; documents/filters.py, PaperlessTaskFilterSet.filter_name 1011–1030. |
| C10 | documents/admin.py, TaskAdmin 146–170 und Registrierung 243; paperless/settings/custom.py, parse_beat_schedule 68–201. |
| C11 | documents/views.py, DocumentViewSet 1009–1029 und get_queryset 1166 ff.; documents/filters.py: CHAR_KWARGS 60, CustomFieldQueryParser 367 ff., _parse_atom 519–576, _get_custom_field 579–595, DocumentFilterSet 847/900/901, PermittedObjectsFilter 1060 ff.; documents/permissions.py 30–53 und permitted_document_ids 460–474. |
| C12 | documents/barcodes.py, BarcodePlugin 82 ff., Kindtask-Publish 213; documents/tasks.py Pluginliste 198–212. |
| N1 | [src/paperless.rs](../src/paperless.rs): upload 78–122; task_status 124–143; find_batch_task 145–210; parse_task 234–271; retry_is_due 274–283; process_batch/reconcile_or_retry/upload_batch 314–380; Worker-Schleife 387–444. |
| N2 | [src/batch.rs](../src/batch.rs): PaperlessReceipt 87–95; open_for_paperless 145–153; create 193–226; record_paperless_* 233–271; add_file 301–322; complete 325–343; write_metadata/update_metadata 451–463. |
| N3 | [src/atomic_file.rs](../src/atomic_file.rs): write 7–35, sync_dir 38–43. |
| B3 | VM /usr/local/sbin/restic-export-apps: Writer-Stop 34, Broker-Stop 39, pg_dump 43–44, Dateiexport 48–52, Neustarts 56–58. Nur gelesen, in diesem Auftrag nicht ausgeführt. |

## 3. Was der aktuelle Pfad tatsächlich garantiert

### Upload und Serverannahme

C1 prüft Authentifizierung und `documents.add_document`, validiert Multipart und liest die Datei in `validate_document` vollständig ein. Dateitypprüfung ist noch kein Beweis erfolgreicher PDF-Verarbeitung. Die View erzeugt ein zufälliges Scratch-Unterverzeichnis, schreibt mit `Path.write_bytes`, setzt mtime und ruft `consume_file.apply_async` auf. Danach liefert sie HTTP 200 mit einer Task-UUID. Kein Client-Schlüssel wird entgegengenommen oder ausgewertet; ein beliebiger zusätzlicher Header allein aktiviert keine Idempotenz.

`write_bytes` schließt die Datei, synchronisiert aber weder Datei noch Verzeichniseintrag ausdrücklich auf den Datenträger. Der persistente Mount hilft gegen Container-Neuerstellung, ersetzt kein fsync. Der heutige HTTP-200-Punkt bedeutet aus Sicht der View „apply_async ist zurückgekehrt“, **nicht** eine atomar haltbare Empfangsoperation mit Datei, DB und Broker. C1/B1.

### Task-Zeile und Broker

`before_task_publish_handler` legt die eindeutige Task-Zeile **vor** dem Publish per get_or_create an. Die Task-ID wird dabei vom Queue-Auftrag bestimmt, nicht von nx-boss. Fehler des Handlers werden gefangen und geloggt, der Handler erzwingt keinen Upload-Abbruch. Deshalb sind beide Richtungen möglich: DB-Zeile ohne Queue-Nachricht oder Queue-Nachricht ohne Task-Zeile. `task_prerun` macht nur UPDATE; `task_postrun` kehrt bei fehlender Zeile zurück. Die Task-Tabelle ist daher keine Outbox. Außerdem enthält input_data primär Dateiname und Overrides, keinen zuverlässig wiederherstellbaren vollständigen Intake-Datensatz mit Scratch-Blob und Anfragefingerabdruck. C2.

Die gelesene Konfiguration und Task-Deklaration setzen für consume_file kein Late-Ack/Worker-Lost-Requeue; Celery-Default ist Early-Ack. Bestätigung beim Worker-Pool-Start kann dem Dokument-Commit vorausgehen. Ein harter Abbruch braucht daher nicht zu automatischer Wiederzustellung zu führen. Bei noch unbestätigten Redis-Nachrichten kann Wiederzustellung auftreten; Kombu hat standardmäßig 3600 Sekunden Visibility-Timeout. Eine Redis-AOF mit everysec ist kein synchroner Commit zusammen mit PostgreSQL. Diese Aussagen beruhen auf C4/C8/B1, nicht auf einem ausgeführten Kill-Test. [Celery-Task-Dokumentation](https://docs.celeryq.dev/en/v5.6.3/userguide/tasks.html), [Redis-Transport](https://docs.celeryq.dev/en/stable/getting-started/backends-and-brokers/redis.html#visibility-timeout), [Redis-Persistenz](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/).

**Aufbewahrung:** `CELERY_RESULT_EXPIRES=3600` betrifft den Redis-Result-Backend-Eintrag. Das ist **nicht** die Lebensdauer von `PaperlessTask` in PostgreSQL. Im untersuchten nicht-testbezogenen Quellbestand und Beat-Plan wurde kein automatischer Task-Historien-Purge gefunden. Acknowledge setzt nur ein Boolean; TasksViewSet ist ansonsten lesend. Die normale Django-Admin-Registrierung hat keine ausdrückliche Löschsperre. Administrative Löschung und Restore können Historie entfernen; externe Wartung außerhalb des untersuchten Codes wurde nicht ausgeschlossen. Keine erfundene „Task-TTL von einer Stunde“. C3/C8/C9/C10.

### Dokument, Hash und Dateien

Preflight berechnet SHA-256 der **Eingangsdatei** und sucht global, einschließlich Papierkorb, nach originaler oder Archiv-Prüfsumme. Bei aktiviertem DELETE_DUPLICATES löscht es die Eingangskopie und gibt einen Duplicate-Fehler mit vorhandener Dokument-ID zurück; sonst wird nur gewarnt und weiter importiert. Die Schalterwirkung gilt auch für API-Uploads, weil sie denselben Consumer verwenden, und übergreift Benutzer/Importquellen. C4/C5.

Diese Prüfung liegt vor Parsing, Pre-Consume-Skript und Dokumenttransaktion. Die später gespeicherte Prüfsumme stammt normalerweise von der möglicherweise veränderten Arbeitskopie; ein Sonderfall erhält das unmodifizierte Original. Selbst eine deterministische Transformation A→B kann den nächsten Eingang A am Vergleich mit gespeichertem B vorbeiführen; die bloße Deterministik behebt diese Hash-Domänen-Lücke nicht. C6: 414–451, 495, 878–893. Die offizielle [Hook-Dokumentation](https://raw.githubusercontent.com/paperless-ngx/paperless-ngx/v3.2.1/docs/advanced_usage.md#pre-consume-script) weist ebenfalls auf Duplikatprobleme bei Veränderungen hin.

**Race:** Worker A und B können beide die Abwesenheit feststellen, dann beide unterschiedliche Dokumentzeilen anlegen. Das spätere MEDIA_LOCK schützt Dateipfade, nicht die vorherige Hash-/Batch-Entscheidung. Weder checksum noch original_filename sind eindeutig; Custom Fields erzwingen nur ein Feld je Dokument, nicht einen eindeutigen Feldwert über Dokumente. Zwei parallele Worker sind ein konkreter Gegenentwurf zur Garantie, auch wenn eine einzelne Worker-Instanz die Wahrscheinlichkeit senkt. C3/C5/C6/B2; SQL-Prüfen-vor-Schreiben ersetzt keine [DB-Eindeutigkeits-/Transaktionsregel](https://www.postgresql.org/docs/18/transaction-iso.html).

Die Dokumenttransaktion beginnt in C6:597. Nach INSERT laufen bereits document_consumption_finished-Signale. Erst danach werden Original, Thumbnail und ggf. Archiv unter dem Medien-Lock geschrieben; `_write` kopiert direkt zum Ziel ohne explizites fsync oder atomaren temporären Publish. `document.save` kann Dateiumbenennungen auslösen. Die Eingangskopie wird **vor Verlassen von transaction.atomic**, also vor dessen erfolgreichem Abschluss, gelöscht. Ein DB-Rollback setzt weder veröffentlichte Dateien noch Unlinks zurück. Eine Datei kann verwaisen, eine Eingangsdatei kann fehlen, und nach Stromausfall kann eine bereits bestätigte DB-Zeile auf unvollständig haltbare Dateien zeigen. C6:673–782, C7 Moves.

Das Task-Ergebnis mit Dokument-ID wird erst nach Rückkehr des Consumers gespeichert. Dazwischen liegt auch das Post-Consume-Skript. Ein Fehler dort oder beim Task-Status-Schreiben kann daher mit einem bereits committed Dokument zusammenfallen. Duplicate-Ergebnisse werden in der Task-Historie ausdrücklich als `failure` gespeichert, obwohl `related_document_ids` die Duplicate-ID liefern kann. Der aktuelle nx-boss-Parser behandelt failure als Fehler und übernimmt diese ID nicht als Erfolg; eine fremde gleiche PDF wäre auch kein Beweis für denselben Batch. C2/C3/C6:793–808/N1.

### Seiteneffekte sind getrennte Commit-Bereiche

- Suchindex und Document-added-Workflows hängen am Signal **innerhalb** der Dokumenttransaktion. Ein Rollback kann bereits erfolgte externe Indexänderungen nicht rückgängig machen.
- Workflow-Webhooks werden direkt mit apply_async veröffentlicht; sie können vor Dokument-Commit loslaufen.
- Der LLM-Index-Auftrag wird ebenfalls direkt veröffentlicht.
- Die Workflow-Aktion „AI suggestions“ verwendet dagegen `delay_on_commit`. Das verhindert einen Publish vor Commit, schließt aber das Fenster **nach DB-Commit und vor erfolgreichem Publish** nicht.
- Das Post-Consume-Skript läuft nach der Dokumenttransaktion.

C6/C7. Eine spätere Outbox muss diese Unterschiede berücksichtigen; „alle Hooks laufen nach Commit“ wäre für den untersuchten Stand falsch.

### Wiederfinden nach verlorener Antwort und Task-Verlust

nx-boss speichert vor POST einen synchronisierten Sendeversuch. Nach Antwort persistiert es die Task-ID, nach success eine Dokument-ID. Fehlt die Task-ID oder findet die UUID-Abfrage nichts, sucht es Tasks über `name=<batch-id>` und prüft lokal den exakten Dateinamen. Unvollständige Pagination wird abgewiesen; unterschiedliche Erfolg-IDs führen zu einem Fehler. Wenn gar kein Task gefunden wird, wird nach 120 Sekunden erneut hochgeladen. **Ein vorhandener pending/started-Task wird dagegen weiter abgewartet, auch wenn seine Broker-Nachricht verloren ist.** Ein vorhandener failure-Task wird als Fehler gespeichert und im normalen UUID-Polling nicht automatisch durch einen neuen POST ersetzt. N1/N2.

Die Suche ist eine PostgreSQL-Filterung von input_data, **keine verzögert aktualisierte Volltextindex-Suche**. Der Zweiminutenabstand ist eine Heuristik, keine aus dieser API ableitbare Sicherheitsgrenze. C9/N1.

Task-Rechte: Modellrecht view_paperlesstask plus Sichtbarkeit: Staff sieht alle Tasks, andere Benutzer eigene und unbesitzte Tasks. Dokument-Rechte werden getrennt gefiltert. Gleicher Dateiname kann von anderen Tasks stammen; Benutzerwechsel, Besitzeränderungen oder eingeschränkte Sichtbarkeit können negative Treffer erzeugen. Abwesenheit für einen Benutzer beweist nicht globale Abwesenheit. C9/C11.

Nach Task-Löschung gibt es aktuell **keine bedingungslos zuverlässige API-Abfrage Batch→Dokument**. Kandidaten für manuelle oder best-effort Reconciliation, ausdrücklich keine neuen laufenden Requests:

```text
GET /api/documents/?original_filename__iexact=<batch-id>.pdf&fields=id,original_filename
GET /api/documents/?custom_field_query=[42,"exact","<batch-id>"]&fields=id,custom_fields
Accept: application/json; version=10
```

Querywerte müssen URL-kodiert werden; 42 ist eine synthetische Feld-ID. `original_filename__iexact` ist das tatsächlich unterstützte Filterformat, nicht ein unterstellter `original_filename__exact`-Filter. Dokumentabfragen respektieren Rechte, blenden regulär Papierkorb aus und listen Root-Dokumente. Feldwerte/Dateinamen sind nicht eindeutig und können entfernt bzw. verändert werden. Ein Custom Field verbessert Wiederfinden nur, wenn es von Anfang an gesetzt, erhalten und sichtbar ist; es reserviert keinen Auftrag vor dem Import. C3/C6:950–962/C11.

Die [offizielle Upload-Dokumentation](https://raw.githubusercontent.com/paperless-ngx/paperless-ngx/v3.2.1/docs/api.md#file-uploads) beschreibt HTTP 200, Task-UUID und spätere Abfrage, aber keinen Idempotency-Key. Die [aktuelle offizielle Duplikat-Konfiguration](https://docs.paperless-ngx.com/configuration/#PAPERLESS_CONSUMER_DELETE_DUPLICATES) und [Nutzungshinweise](https://docs.paperless-ngx.com/usage/#duplicate-documents) stimmen mit dem gelesenen Consumer überein. Direkter Abruf der drei docs-Seiten gab zunächst 403; API-/Hook-Dokumentation wurde über das offizielle versionsgebundene Repository gelesen, die Konfiguration zusätzlich über den Suchindex der offiziellen Seite. Code und DB-Schema sind hier die stärkeren Belege.

## 4. Ist-Ablauf

Die vertikalen Systemgrenzen sind echte Persistenzgrenzen. Kein gemeinsamer Commit umfasst nx-boss, PostgreSQL, Redis und Medienverzeichnis.

```mermaid
sequenceDiagram
    participant N as nx-boss
    participant L as Lokale Batch-Dateien
    participant API as Paperless Upload-API
    participant F as Scratch / Medien
    participant DB as PostgreSQL
    participant R as Redis / Celery
    participant W as Consumer
    participant X as Index / Webhook / KI
    N->>L: PDF und Sendeversuch atomar schreiben und syncen
    N->>API: POST PDF, Filename Batch-ID.pdf, Metadaten
    API->>F: Scratch-Datei schreiben, kein explizites fsync
    API->>DB: before_task_publish: Task pending anlegen
    Note over DB,R: Getrennte Writes; DB-Fehler wird abgefangen
    API->>R: consume_file publish
    API-->>N: HTTP 200 mit Task-UUID
    N->>L: Task-ID atomar speichern
    R->>W: Zustellung; standardmäßig frühes ACK
    W->>DB: Task started; SHA-256-Duplikatsuche
    W->>W: Workflows, Arbeitskopie, Pre-Skript, Parsing
    W->>DB: BEGIN; Dokument anlegen
    W->>X: Signale; Index, Webhook / LLM-Publish
    W->>F: Finale Dateien kopieren / verschieben; Scratch löschen
    W->>DB: COMMIT
    W->>X: on-commit KI-Aktion; Post-Consume-Skript
    W->>DB: Task-Ergebnis mit Dokument-ID speichern
    N->>API: GET Task / bei Unklarheit Dateinamen-Suche
    API-->>N: Task-Status und Dokument-ID
    N->>L: Dokument-ID atomar und synchronisiert speichern
```

## 5. Crash- und Race-Matrix des Ist-Zustands

Jede Zeile betrachtet den Schnitt unmittelbar vor/nach dem genannten Ereignis. A = aus den belegten Abläufen abgeleitet, nicht praktisch injiziert. „Datei vorhanden“ bedeutet bei fehlendem fsync nach Stromverlust nicht zwingend haltbar. Referenzen verweisen auf Abschnitt 2.

| Grenze / Störung | Nach Neustart möglicher dauerhafter Zustand | Sicht von nx-boss und Risiko | Beleg / Abschluss im Entwurf |
| --- | --- | --- | --- |
| Vor/nach lokalem PDF-/Intent-Sync | Vor Erfolg alter Metadatenstand; nach Erfolg PDF und Intent, noch keine Server-ID | Wiederöffnen/Retry erforderlich; lokale Atomic Writes lösen kein Server-Antwortproblem | N2/N3; E1/E7 |
| HTTP-Body angenommen, vor Scratch-Schreiben | Nur Client-Intent, eventuell serverseitiger Request-Tempzustand | Keine Task-Zuordnung; erneuter POST nötig | C1; E1 |
| Während/nach Scratch-Schreiben, vor DB-Zeile | Vollständige, partielle oder verwaiste Scratch-Datei; kein Auftrag | Retry legt neue Scratch-Datei an; alte wird nicht über einen Empfangsschlüssel gefunden | C1; E1/E2 |
| Task-Zeile committed, vor Broker-Publish | pending-Zeile + eventuell Datei, keine Queue-Nachricht | Suche findet pending; Client wartet unbegrenzt statt den verlorenen Publish zu reparieren | C2/N1; E2/E3 |
| Task-Zeilenanlage scheitert, Publish gelingt | Queue/Datei, keine sichtbare Task-Zeile | Verarbeitung kann trotzdem laufen; nach 120 s kann Client nochmal posten | C2/N1; E2 |
| Broker nimmt Nachricht an, Producer verliert Bestätigung | Task ggf. pending, Publish-Ergebnis unklar, mögliche erneute Publikation | Gleiche oder neue Zustellung möglich; Task-ID ist keine Ausführungssperre | C1/C2/C8; E3/E4 |
| Publish zurück, vor/nach HTTP 200; Antwort verloren | Serverseitiger Auftrag kann existieren, Client hat nur Intent | Exakte Task-Suche kann helfen; falls unvollständig/fehlend kein Beweis gegen laufenden Import | C1/N1; E5/E7 |
| HTTP-Antwort da, vor lokalem Task-ID-Sync | Server-ID bekannt im RAM, auf Disk nur Intent | Nach Neustart wieder gleiche Unklarheit | N1:377–379/N3; E5/E7 |
| Vor Worker-Start / nach frühem ACK | Vor ACK ggf. Redelivery; nach ACK Task kann ohne Wiederzustellung abbrechen | pending/started oder failure bleibt; bloßes Container-Neustarten repariert keinen Empfangsauftrag | C4/C8; E3/E4 |
| Während Hash-Suche / Pre-Skript / Parsing | Eingang ggf. vorhanden, kein Dokument; externe Vorarbeit möglich | Fehler/started; erneute Ausführung kann Skript-/OCR-Effekte wiederholen | C4–C7; E4/E6 |
| Zwei Worker passieren Duplikatprüfung zugleich | Beide sehen keinen committed Treffer | Beide können je eine Dokumentzeile anlegen; Media-Lock greift zu spät | C3/C5/C6/B2; E4 |
| Dokument-INSERT vor Transaktionsende | Noch nicht committed; Rollback entfernt DB-Zeile | Eventuell schon Suchindex/Queue-Effekte; Client sieht noch keine bestätigte ID | C6/C7; E4/E6 |
| Vor/nach finalem Copy oder Rename, noch vor Commit | Partielle/verwaiste Medien oder neuer Dateipfad, DB kann zurückrollen | DB-Transaktion stellt Dateien nicht zurück; Replay kann neue Dateireste erzeugen | C6/C7; E4 |
| Eingang gelöscht, vor erfolgreichem DB-Commit | Eingang fehlt, DB-Zeile fehlt bei Rollback; evtl. finale Dateien verwaist | Derselbe Celery-Auftrag kann FILE_NOT_FOUND melden; Task-Tabelle enthält keine haltbare Wiederaufnahme | C6:765–784; E4 |
| DB-Commit zurück, Datenträger verliert ungesyncte Datei | Dokumentzeile existiert, Original/Thumbnail/Archiv fehlt oder ist unvollständig | Hash-Prüfung kann Retry trotzdem ablehnen; ID allein bestätigt keine lesbare Datei | C6/B1; E4 |
| Workflow-/Webhook-/Index-Publish vor Commit | Externe Aufgabe bereits veröffentlicht oder ausgeführt; DB später zurückgerollt | Wiederholte oder auf nicht sichtbares Dokument gerichtete Effekte | C7; E6 |
| DB-Commit vor on-commit-Publish / Post-Skript | Dokument existiert; Folgeauftrag fehlt oder Skript scheitert | Task kann failure werden, obwohl Dokument existiert; unklarer externer Erfolg | C6/C7; E4/E6 |
| Dokument fertig, vor/nach Task-Ergebnis-Speicherung | Dokument vorhanden; Task ggf. started/failure/fehlend statt success | Client findet Dokument-ID nicht zuverlässig; erneuter POST kann Duplicate-Fehler oder zweites Dokument ergeben | C2/C3/C6; E4/E5 |
| GET-Ergebnis verloren / vor lokalem Dokument-ID-Sync | Task success bleibt ggf. verfügbar, Client wartet weiter | Wiederholtes GET normalerweise sicher; nach Historienverlust wieder Unsicherheit | N1/N2; E5/E7 |
| Task administrativ gelöscht / Historie per Restore verloren | Dokument kann bleiben, UUID/Dateinamen-Task nicht mehr auffindbar | Nach 120 s erneuter POST; gleiche Bytes können nur failure ergeben; globale andere Treffer möglich | C3/C9/C10/N1; E5/E8 |
| Redis-Ausfall/AOF-Verlust | PostgreSQL und Dateien können neuer als Queue sein; ACK/Publish kann verloren sein | pending/started hängt oder redelivered; kein verlässlicher Reconciler | C2/C8/B1; E3 |
| PostgreSQL-Ausfall beim Empfang/Finalisieren | Publish kann trotz Task-DB-Fehler gelingen; Dokumenttransaktion kann scheitern | Verlorene Statusinformation, Dateireste, möglicherweise schon externe Effekte | C2/C6/C7; E2/E4 |
| Datenträger voll / Datei beschädigt oder verschwunden | Client-Intent oder Server-Datei kann nicht sicher gespeichert sein | Fehler behalten; keine erfolgreiche Freigabe allein aufgrund einer Hashzeile | N3/C1/C6; E1/E4 |
| Gleichzeitige identische POSTs / zwei nx-boss-Prozesse | Pro POST zufällige Scratch-Datei und eigene Task-ID; kein gemeinsamer Claim | Zwei-Minuten-Fenster und Prozess-Serialisierung sind keine serverweite Sperre | C1/N1; E2/E4 |
| Gleiche Batch-ID, andere Bytes/Tags/Titel | Aktuell kein Server-Request-Fingerprint | Dateinamen-Suche kann falschen Auftrag übernehmen; Inhaltsprüfung erfasst Metadaten nicht | C1/C5/N1; E2/E5/E7 |
| Gleiche Bytes, unterschiedliche Batch-IDs / anderer Besitzer | Globale Duplicate-Suche kann vorhandenen fremden/gelöschten Datensatz treffen | Legitimer neuer Batch abgewiesen; Duplicate-ID beweist keine eigene Batch-Zuordnung | C3/C5/C9; E5, Produktentscheidung |
| Neustart beider Container / inkonsistenter Backup-Restore | Client-Receipt, DB, Queue und Dateien können verschiedene Zeitpunkte zeigen | Bestätigte ID kann fehlen; alte Queue kann Operationen erneut auslösen | B3/N1/C8; E3/E8 |

## 6. Lösungswege im Vergleich

| Weg | Was er verbessert | Konkrete verbleibende Lücke | Bewertung |
| --- | --- | --- | --- |
| Idempotency-Key + eindeutige Empfangstabelle, Request-Fingerprint, Outbox, wiederaufnehmbarer Consumer | Atomare logische Annahme, Konflikterkennung, dauerhaftes Wiederfinden | File/DB-Publish und Worker-Fencing müssen zusätzlich gelöst werden; externe Effekte separat | **Empfohlen**, mit E1–E8 |
| Idempotency-Key nur in Header/Cache oder Tabelle, danach unverändertes apply_async | Wiederholte HTTP-Aufrufe teilweise erkennbar | DB-/Redis-Lücke, Worker-Crash, Dokument-vor-Status-Commit bleiben | Nicht ausreichend |
| Eindeutiger Batch-Schlüssel direkt am Dokument | Kann doppelte committed Dokumentzeilen verhindern | Vor Dokumententstehung kein Status/Intake; Payload-Konflikte und Queue-Verlust ungelöst; Hooks vor Unique-Verletzung möglich | Sinnvolle zweite Sperre, nicht alleinige Lösung |
| Normales Paperless-Custom-Field | Dauerhafterer Suchhinweis als Task-Historie | Nur (document,field) eindeutig; Werte editierbar; erst nach Dokumentanlage; Rechte/Trash | Reconciliation-Hilfe |
| Globaler Content-Hash / DELETE_DUPLICATES | Sequenzielle identische Eingänge häufig abweisen | Kein Unique-Index, Race, andere Hash-Domäne nach Transformation, gleiche Bytes verschiedener Batches/Benutzer, failure statt eigener Bestätigung | Inhaltsregel, kein Idempotenzprotokoll |
| Deterministische Celery-Task-ID | Gemeinsame Korrelations-ID | get_or_create schützt nur Task-Zeile; dieselbe ID kann mehrfach ausgeführt werden; Ergebnisüberschreiben/TTL | Nur Diagnose-/Korrelationshilfe |
| Dateinamen-/Custom-Field-Suche vor Retry | Verlorene Antwort oft rekonstruierbar | TOCTOU zwischen Suche und POST; Historie/Sichtbarkeit/gleichnamige Uploads; andere Metadaten unbemerkt | Best effort |
| Nur Client-Intent, längeres Warten, Single-Worker | Weniger parallele Wiederholungen | Keine Entscheidung über „Server hat schon committed“ möglich, wenn Antwort fehlt | Kann Safety nur durch Nicht-Wiederholen erkaufen, verliert dann Liveness |

Die Forderung „höchstens einmal und bei jeder unklaren Antwort automatisch weiter“ kann ohne serverseitige Unterstützung nicht aus bloßer Abwesenheit eines Suchtreffers hergeleitet werden. Die beiden Fälle „nie angenommen“ und „angenommen, Ergebnis/Antwort nicht sichtbar“ sind für den Client gleich beobachtbar.

## 7. Empfohlener Entwurf: dauerhafte Importoperation

**Alles in diesem Abschnitt ist vorgeschlagen, nicht vorhanden oder umgesetzt.** Kein neuer externer Dienst ist zwingend nötig: PostgreSQL plus ein Recovery-Job innerhalb der Paperless-Anwendung kann genügen. Redis wird Zustellungsbeschleuniger, nicht alleiniger Nachweis angenommener Arbeit.

### E1 – Haltbare Eingangskopie und eingefrorene Anfrage

Vor der Annahme die vollständige Datei in einen privaten Intake-Bereich schreiben, SHA-256 und Größe prüfen, Datei fsyncen, unter unveränderlichem Blob-Namen veröffentlichen und das betroffene Verzeichnis fsyncen. Temporäre konkurrierende Uploads bekommen getrennte Dateien; gleiche Schlüssel dürfen keine Datei überschreiben.

Fingerprint-Version plus canonical request enthalten mindestens: Datei-Hash/Größe, normalisierter Originalname und MIME-Typ, Titel, created, Tags, Correspondent/Document-Type/Storage-Path, ASN, Custom-Fields, angeforderte Besitzer-/Rechtewerte und relevante API-Optionen. Reihenfolge nur dort normalisieren, wo sie semantisch unerheblich ist; null, fehlend und Default bewusst definieren. Multipart-Grenzen, Tokenwert und HTTP-Request-ID gehören nicht hinein.

Serverseitige Defaults und Importprofil beim **ersten** Auftrag auflösen und unveränderlich speichern. Wiederholung gegen dieselbe Version/Erstauflösung vergleichen, nicht gegen mittlerweile geänderte globale Defaults. Transformationen dürfen den ursprünglichen Request-Hash nicht ersetzen. Ein neues Profil braucht bewusste neue Operation.

Crash nach Blob-Publish vor DB-Commit hinterlässt höchstens einen verwaisten Blob. Garbage Collection muss aktive Uploads/Operationen schützen und erst nach ausreichender Quarantäne löschen; Datum allein ist kein sicherer Live-Referenztest.

### E2 – Atomare Empfangsentscheidung in PostgreSQL

Neue Tabelle etwa ImportOperation:

- Nicht-null Namensraum + Schlüssel, **UNIQUE(namespace_id, client_key)**; Namensraum dauerhaft und bei Benutzerlöschung nicht stillschweigend NULL/neu zugeordnet.
- Eindeutige operations_uuid, fingerprint_version/request_fingerprint, Intake-Blobreferenz, persistierte effektive Metadaten.
- Zustand accepted/queued/processing/finalizing/succeeded/failed/deleted und zusätzliche error_class/retryable, Zeiten, Attempt-/Lease-Generation.
- Dauerhaftes Resultat bzw. ursprüngliche document_id als Tombstone-Information; nicht per Dokument-CASCADE mitlöschen.
- Separate Attempts für technische Task-IDs; Fehler einer alten Generation darf ein fertiges Ergebnis nicht überschreiben.

Nach haltbarer Datei in **einer DB-Transaktion** Operation und eindeutigen Outbox-Auftrag anlegen. Erst deren erfolgreicher Commit ist „dauerhaft angenommen“. Ein fehlgeschlagener Task-Tracking-Write darf hier nicht ignoriert werden. Bei Unique-Konflikt Gewinnerzeile lesen: gleicher Fingerprint → bestehende Operation; anderer → 409. Keine Prüfen-dann-Einfügen-Entscheidung ohne DB-Constraint.

Für Dokumente zusätzlich eindeutige, nicht editierbare operation_id-Zuordnung, z. B. ein nullable **UNIQUE FK** auf ImportOperation (für Legacy-Dokumente leer). Dokumentanlage und Zuordnung dürfen nicht zeitlich getrennt werden. Benutzer- oder Dateinamensänderungen ändern K nicht. Ein späteres Dokument-Delete darf diese Operation nicht zur Wiederverwendung freigeben.

### E3 – Outbox und Wiederaufnahme statt Vertrauen auf Queue-Publish

Outbox-Dispatcher veröffentlicht die Operations-ID. Ein Crash zwischen Publish und „gesendet“ kann dieselbe Nachricht erneut senden: das ist vorgesehen. Ein „gesendet“-Flag beweist nicht dauerhaften Brokerbesitz; der Recovery-Job muss auch accepted/queued-Operationen ohne Fortschritt erneut aktivieren können.

Worker claimt die Operation in PostgreSQL; Ablauf einer Lease erlaubt Recovery, aber genügt allein nicht gegen einen pausierten alten Worker. Eine monoton erhöhte **Fencing-Generation** muss bei jeder Finalisierung unter DB-Lock geprüft werden. Neue Versuche bekommen separate Arbeitsverzeichnisse und technische Task-IDs. Wiederholte Queue-Zustellung erzeugt keine neue Operation und keine neue Dokumentidentität. Bei Redis-Neustart findet Recovery offene Arbeit in PostgreSQL wieder. Late-Ack/Requeue wären erst nach dieser Absicherung sinnvoll; sie ersetzen sie nicht.

PaperlessTask bleibt eine UI-/Diagnoseansicht. Vorhandene Felder owner/status/input_data/result_data helfen beim Darstellen, enthalten aber weder vollständiges Intake noch unveränderlichen Fingerprint, Claim/Fencing oder dauerhafte Outbox. Eine separate ImportOperation ist klarer als das allgemeine Task-Modell zu einem nur für Uploads geltenden Ledger umzubauen.

### E4 – Wiederaufnehmbare Dokument-/Datei-Finalisierung

Parsing/OCR darf vor der kurzen Finalisierung stattfinden und erneut laufen. Alle erzeugten Dateien zunächst pro Attempt privat vorbereiten und synchronisieren. **Keine sichtbaren Dokument-INSERTs mit externen Signals/Hooks als Nebenwirkung** während dieser Vorbereitungsphase.

Beim Finalisieren Operation sperren, Generation und terminalen Zustand erneut prüfen. Gleichzeitige alte Worker dürfen weder DB-Ergebnis noch Gewinnerdateien überschreiben. Dafür Dateien unter attempt-spezifischen unveränderlichen Namen vorbereiten; ein alter Worker kann höchstens quarantänisierte, unreferenzierte Dateien hinterlassen. Der gültige Versuch veröffentlicht vollständig gesyncte Assets unter eindeutigen Pfaden und legt **in derselben DB-Transaktion** Dokument, operation_id, Ergebniszustand und Folgeeffekt-Outbox an. Verzeichnisse bei Rename/Move auf beiden Seiten korrekt synchronisieren; Cross-Filesystem-Copy benötigt eigene sichere Publish-Logik.

Ein DB-Commit nach dem File-Publish kann scheitern: dann bleiben nur nicht referenzierte Assets, die Recovery erkennen/prüfen oder später bereinigen darf. Der ursprüngliche Intake-Blob bleibt bis zur gesicherten Finalisierung erhalten. **Kein Unlink des einzigen wiederverwendbaren Eingangs vor DB-Commit.** Vorhandene Consumer-_store/Signale müssen dafür umgebaut werden; eine zusätzliche atomic-Klammer allein löst nichts.

Nach Erfolg bleibt derselbe Datensatz maßgeblich. Fehlt später eine Datei, Zustand beschädigt/reparaturbedürftig melden und gegebenenfalls **dieselbe** Dokumentidentität aus gesichertem Eingang reparieren, keinen zweiten Import anlegen. Vollständiger Verlust aller Kopien verletzt die Verfügbarkeitsannahme und darf nicht als Erfolg ausgegeben werden.

Die Safety-Begründung benötigt beide Sperren: eindeutige Empfangsoperation plus atomare Dokumentzuordnung; Fencing verhindert, dass alte Worker spätere Zustände überschreiben. Unveränderliche Assets entkoppeln Dateireste von sichtbaren Dokumenten. Eine globale Prüfsummen-Unique-Constraint wäre hierfür fachlich falsch.

### E5 – Vorgeschlagenes API-Verhalten und Rechte

Neue, explizit versionierte Import-API oder ausgehandelter Idempotenz-Modus; keine stille Bedeutungsänderung des bisherigen v10-200-UUID-Vertrags. Beispiel: POST /api/imports/ mit Idempotency-Key, GET /api/imports/{operation_id}/ und optional authentifizierte Abfrage per Client-Schlüssel.

| Anfrage/Zustand | Vorgeschlagene Antwort |
| --- | --- |
| Erst-POST erfolgreich haltbar angenommen | 202 mit stabiler operation_id, status_url, Fingerprint-Version und Retention-Vertrag; noch keine garantierte Dokument-ID. |
| Gleichzeitiger identischer POST | Nach DB-Unique-Entscheidung dieselbe Operation; keine zusätzliche logische Verarbeitung. |
| Gleicher Schlüssel, anderer relevanter Payload | 409 idempotency_conflict, auch wenn der Erstauftrag failed ist. Keine fremden Requestdetails offenlegen. |
| accepted/queued/processing/finalizing | 202 bzw. GET 200 mit Zustand und Retry-After; keine neue Operation. |
| succeeded | POST-Replay/GET liefert dasselbe Ergebnis mit derselben document_id; Blob/Hashes werden nicht aus Benutzerdateinamen geraten. |
| Transienter Fehler | Recovery arbeitet innerhalb derselben Operation weiter; Retry-Zähler/Fehlerklasse im Status. |
| Dauerhaft ungültiger Import / fachlich abgewiesen | Derselbe terminale failed-Status; beim POST-Replay z. B. 422. Korrigierte Eingabe braucht neuen Schlüssel. |
| Absichtlich gelöschtes Dokument | 410/deleted mit dauerhaftem Tombstone; nie automatisch neu importieren. Original-ID nur bei berechtigtem Zugriff. |
| Rechte entzogen / falscher Namensraum | 403 bzw. nicht enthüllendes 404 nach festem Vertrag; niemals automatisch in anderem Scope neu anlegen. |
| Unbekannter Schlüssel nach abgelaufener Garantie oder Restore-Unklarheit | Explizite expired/reconciliation_required-Antwort; nicht still als frischen Auftrag behandeln. |

Die stabile Identität ist **Operation**, nicht eine beliebige Celery-Ausführung. Ein erster und ein Recovery-Versuch dürfen unterschiedliche technische Task-IDs haben. Falls Kompatibilität eine wiederholbare „Task-ID“ verlangt, eine dauerhaft zugeordnete logische ID/Ansicht anbieten und Attempts separat zeigen. Keine alleinige Bindung an Redis-TTL oder löschbare PaperlessTask-Zeilen.

Scope ist eine langlebige Integration bzw. ein Benutzer/Mandant mit stabiler Identität, nicht das rotierbare API-Token. Tokenrotation im gleichen Scope erhält die Zuordnung. Benutzerwechsel/Accountlöschung braucht eine ausdrückliche Übernahme-/Tombstone-Regel. Statusabfragen müssen den Scope autorisieren; allgemeine Task- oder Dokument-Sichtbarkeit darf weder Schlüsselkonflikte umgehen noch IDs anderer Benutzer offenlegen.

### E6 – Folgeeffekte mit eigener Outbox

Suchindex-/KI-/Webhook-Aufträge als eindeutige Effektzeilen, etwa UNIQUE(operation_id, effect_type, workflow_revision, ordinal), gemeinsam mit dem Dokument-Commit speichern. Dispatcher mit Wiederholungen; lokale Index-Upserts auf Dokument-ID idempotent gestalten. Einen nicht-idempotenten externen Aufruf nach Antwortverlust nicht als beweisbar genau einmal darstellen.

Webhooks erhalten stabile Event-ID und dokumentierten Replay-Vertrag. Der Empfänger muss Deduplizierung und seine Fachänderung selbst atomar speichern, sonst bleibt der Effekt mindestens-einmal oder bei unterdrücktem Retry möglicherweise verloren. Gleiches gilt für externe KI-/OCR-Anbieter und Abrechnung. Ein outbox.sent-Flag allein beseitigt das Antwortverlustfenster nicht.

Pre-Consume-Skripte für den garantierten Pfad auf wiederholbare Transformationen ohne unkontrollierte externe Effekte beschränken oder diese Effekte ausdrücklich ausnehmen. Nichtdeterministische Transformation: zuerst vollständig gespeichertes Transformationsergebnis wiederverwenden; nach Crash vor Speicherung kann Berechnung erneut abweichen, die Operation bleibt aber dieselbe. Keine Skripte dürfen unabhängig neue Dokumente anlegen. Split/Collate im ersten garantierten Modus deaktivieren/ablehnen; ein späteres Mehrdokument-Modell bräuchte eindeutige Kindoperationen statt einer willkürlich ausgewählten ersten Dokument-ID.

### E7 – Änderungen an nx-boss

- Stabile Batch-ID weiterverwenden; Scope/Server-Instanzidentität und vollständige relevante Requestdaten samt Hashversion/Fingerprint vor dem ersten POST dauerhaft speichern. Der aktuelle Receipt enthält noch keinen solchen vollständigen Request-Snapshot.
- Insbesondere Tags/Custom-Field-Konfiguration nicht bei jedem Retry aus inzwischen geänderter Live-Konfiguration übernehmen.
- Nach Timeout dieselbe Operation per Schlüssel/ID abfragen oder denselben POST wiederholen; die garantierte Variante benötigt keine Dateinamen-Heuristik und kein blindes 120-Sekunden-Neuanlegen.
- operation_id, technische Attempts, document_id, terminale Fehler, deleted/expired/reconciliation_required ausdrücklich persistieren. Bestehende atomare Datei-/Verzeichnis-Syncs behalten.
- Ein Erfolg muss genau eine valide, zur Operation gehörige Dokument-ID bestätigen; lokale Freigabe weiterhin erst nach dauerhafter Receipt-Speicherung.
- Legacy-Server nur in ausdrücklich best-effort genanntem Modus bedienen. Fehlende Capability, 404/403 oder API-Fehler dürfen keinen stillen Downgrade der zugesagten Garantie auslösen.
- Vorhandene mehrdeutige Alt-Receipts nicht durch neue Schlüssel erneut posten. Erst zuordnen oder als ungeklärt markieren. API plus Consume-Ordner für dieselbe garantierte Lieferung verhindern.

### E8 – Aufbewahrung, Backups und Recovery

Für unbegrenztes Wiederholen: minimale nicht wiederverwendbare Schlüssel-/Fingerprint-/Ergebnis- oder Tombstone-Zeile dauerhaft behalten. Größere Request-/Datei-/Attemptdaten dürfen nach festgelegten Regeln ausgedünnt werden. Ein Löschauftrag für das Dokument darf nicht automatisch den Idempotenzbeleg löschen. Datenschutz-/Speicheranforderungen sind eine Produktentscheidung; wer das gesamte Register entfernt, beendet damit die Garantie.

Bei endlichem Horizont T gilt die Zusage nur innerhalb T; alte Schlüssel müssen als abgelaufen erkennbar bleiben oder Clients müssen nach Ende ausdrücklich auf manuelle Reconciliation wechseln. Ohne irgendeinen Restbeleg kann der Server einen alten Schlüssel nicht von einem neuen unterscheiden. Alte Client-Backups können älter sein als jede gewählte Task-TTL.

Der heutige App-Export stoppt Writer und Redis und sichert PG-Dumps plus Dateien (B3); das ist eine gute Ausgangsbasis für einen konsistenten Checkpoint, aber kein globales Log externer Effekte. Ein künftiger Restore muss Dokumente, Operationen/Tombstones, Intake/Assets, Outbox und Scanner-Receipts zeitlich abstimmen. Queue-Nachrichten nach Restore nur anhand der DB-Operation ausführen; veraltete Attempts durch Generation/Restore-Epoch abweisen.

**Unmöglichkeitsgrenze:** Ein Restore auf einen Zeitpunkt vor einem früher bestätigten Import kann dessen Existenz vergessen, während ein Webhook-Empfänger oder nx-boss den Erfolg noch kennt. Ohne zusätzliches nicht zurückgerolltes Journal und ausreichende Nutzdatenkopien lässt sich keine historische globale Genau-einmal-Zusage über diesen Verlust hinweg halten. Unbekannte alte Receipts dürfen nicht blind neu importiert werden. Restore-Fence/neue Recovery-Epoch mit angehaltener automatischer Zustellung und ausdrücklicher Reconciliation vorsehen. Auch vor Restore vergebene numerische Dokument-IDs können später anders belegt sein; deshalb nie nur eine wieder existierende Zahl als Beweis akzeptieren.

## 8. Migration, Rollout und andere Importe

**Paperless-Arbeit:** neue Tabellen/Constraints für ImportOperation, Outbox und Attempts; eindeutige Dokumentzuordnung; Intake-Persistenz; geschütztes Status-API; Claim/Fencing; wiederaufnehmbare Consumer-Finalisierung; getrennte Follow-up-Outbox; Tombstones, Retention und Restore-Fence. Das ist mehr als ein Header-Patch.

**nx-boss-Arbeit:** versionierter persistenter Request/Receipt, Capability-Negotiation, Statusmaschine und Retries auf dieselbe Operation, explizite Altfall-Reconciliation und kein Doppelweg über Consume.

**Migration:** additive Tabellen, nullable Dokumentzuordnung für Altbestand. Keine Zuordnung aus Dateinamen/Hashes als „bewiesen“ massenhaft übernehmen. Eindeutige Altbelege können geprüft migriert werden, Mehrdeutigkeiten bleiben offen. Bestehende Clients behalten den bisherigen Uploadvertrag; der garantierte Pfad wird zunächst separat angeboten.

**Reihenfolge:** isolierte Tests → serverseitige additive Migration/API/Recovery → alle beteiligten Worker auf neue Fencing-/Finalisierungslogik → Capability freigeben → nx-boss umstellen → offene Alt-Receipts behandeln. Gemischte alte/neue Consumer dürfen garantierte Aufträge nicht verarbeiten. Rückrollgrenze: neue Aufträge nicht durch einen alten Worker ausführen; bei Rollback garantierte Annahme pausieren und Ledger erhalten, nicht Tabellen/Zuordnung löschen.

Die globale DELETE_DUPLICATES-Einstellung wurde hier **nicht geändert**. Sie betrifft Mail-, Ordner-, Web- und API-Importe und weist auch absichtlich identische Dateien anderer Batches ab. Empfehlung für den neuen Vertrag: Idempotenz unabhängig davon implementieren; Inhaltsduplikat bleibt ein eindeutiger fachlicher Fehler oder eine ausdrücklich gewählte, berechtigte Zuordnung zu vorhandenen Inhalten. Ein solcher vorhandener Inhalt darf nicht still als „derselbe Batch“ gelten. Wer zwei verschiedene identische Batches beide als Dokumente speichern will, braucht eine bewusste Änderung dieser Inhaltsregel; das ist kein notwendiger Bestandteil des Idempotenz-Fixes.

## 9. Späterer TDD-/Fault-Injection-Plan

**Hier nur geplant, nicht ausgeführt.** Separater Teststack mit synthetischen PDFs, privaten Testvolumes, eigener DB/Redis, Mock-KI und Mock-Webhooks; keine produktiven Tokens oder Datensätze. Für Persistenztests reicht SIGKILL allein nicht: das simuliert keinen Verlust ungesyncter Kernel-/Storage-Daten. Zusätzlich isolierte VM-/Blockdevice-Faults oder ein kontrolliert fehlschlagendes Storage-Backend verwenden.

Gemeinsame Assertions: Eine Operation je K; höchstens eine veröffentlichte Dokumentidentität je K; Request-Konflikte ändern den Gewinner nicht; gleiche Wiederholungen liefern dieselbe Operation; terminale Zustände werden nicht von alten Attempts überschrieben; Erfolg impliziert lesbare, gehashte Originaldatei; fehlende/kaputte Daten ergeben keinen falschen Erfolg. HTTP-, Worker-, Dokument- und externen Effekt-Zähler getrennt erfassen.

| Test | Gezielter Schnitt / Eingabe | Erwartung |
| --- | --- | --- |
| Parallelität | 2/20/100 identische POSTs und mehrere Worker, Barrier vor Claim/Commit | Eine Operation und ein Dokument; technische Attempts dürfen mehrfach sein. |
| Konflikte | Gleiches K mit anderen PDF-Bytes, Titel, Tags, Datum, Custom Fields; andere Multipart-Grenze bei gleicher Semantik | Relevante Änderungen 409; semantisch identische Darstellung gleicher Replay. |
| Verlorene Antworten | HTTP-Antwort nach Annahme abschneiden; lokales Task-/Receipt-Schreiben scheitern lassen | Wiederfinden anhand K, kein neuer logischer Import. |
| Intake-Crash | Vor/nach write, file-fsync, rename, dir-fsync, DB-Empfangscommit | Vor Annahme Retry möglich; nach Annahme Blob haltbar; sichere Orphan-GC. |
| DB-/Broker-Lücke | Kill vor/nach Outbox-Commit, Publish, Publish-Bestätigung, Dispatcher-Markierung | Recovery veröffentlicht verlorene Arbeit; doppelte Nachrichten sicher. |
| Redis-Neustart | Redis stoppen/leeren im isolierten Stack, bestätigte Queue-Daten verlieren | DB-basierte Recovery schließt offene Operation ab; kein neues Dokument. |
| Worker-Crash | Vor/nach frühem/spätem ACK, Parsing und Claim-Erneuerung | Recovery; alter Worker mit abgelaufener Generation darf nicht finalisieren. |
| Zwei Generationen | Alten Worker pausieren, Lease übernehmen, alten anschließend fortsetzen | Nur gültige Generation kann Ergebnis/Assets sichtbar machen. |
| DB-Fehler | Verbindungsverlust/Deadlock/Commit-Antwortverlust beim Empfang und Finalisieren | Eindeutigen Zustand erneut lesen; keine doppelte Dokumentzuordnung. |
| Medien-Finalisierung | Kill vor/nach Asset-Publish, Dateiumbenennung und DB-Commit; ENOSPC/EIO | Entweder kein sichtbarer Erfolg oder vollständiges Dokument; Eingangsblob für Recovery erhalten. |
| Pre-Consume | Deterministische A→B und nichtdeterministische A→B/C-Transformation | Eingangs-Fingerprint stabil; höchstens eine Dokumentidentität; wiederholte Skript-/KI-Effekte separat gemessen. |
| Fehler/Abbruch/Split | Ungültiges PDF, Workflow-Stop, revoked, Barcode-Split/Collate | Dauerhaft klare Fehler bzw. garantierter Pfad lehnt Mehrdokument-Modus ab; keine falsche Einzel-ID. |
| Task-Aufräumen | Task- und Redis-Ergebnisse nach Erfolg und während Verarbeitung entfernen | Operation bleibt auffindbar und Recovery arbeitsfähig. |
| Sichtbarkeit | Zwei Benutzer, gleiche Schlüssel, Tokenrotation, Besitzer-/Rechtewechsel | Namensräume isoliert; kein Datenleck und kein Retry in falschem Scope. |
| Löschen/TTL | Soft-/Hard-Delete eines Dokuments, Schlüssel jenseits T, sehr altes Client-Backup | Tombstone/expired statt Neuimport; definierte Wiederherstellungsentscheidung. |
| Gleiche Bytes | Verschiedene Batch-IDs, existierendes fremdes Dokument, Papierkorb | Gewählte Inhaltsregel, niemals falsche Batch-Gleichsetzung. |
| Dateiverlust | Original nach success entfernen/beschädigen im Teststack | Reparatur derselben ID oder beschädigt-Status; kein zweites Dokument. |
| Backup-Restore | Gemeinsam älterer Checkpoint; absichtlich auseinanderlaufende DB-/Asset-/Client-Stände | Konsistenter Fall rekonstruierbar; Verlust des bestätigten Suffixes löst Restore-Fence/Reconciliation aus. |
| Externe Effekte | Antwortverlust nach erfolgreichem Mock-Webhook/Mock-KI; Empfänger mit/ohne Event-Deduplizierung | Grenze sichtbar: mit kooperierendem Empfänger ein Fachereignis, sonst Doppelaufruf/-kosten möglich. |
| Legacy/Rollout | v10-Client ohne Schlüssel, neue/alte Worker, fehlende Capability | Altes Verhalten erhalten; keine fälschliche Garantie oder stille Herabstufung. |

Vor produktiver Freigabe die bestehenden nx-boss-Tests, HTTP-Mocks, Fmt/Clippy, Shutdown-/Logging- und Compose-Checks sowie passende Paperless-Migrations-/API-/Consumer-Tests ausführen. Dieser reine Forschungsauftrag hat keine dieser zustandsverändernden Tests gestartet.

## 10. Offene Fragen und sichere Klärung

| Frage | Benötigte Beobachtung / Entscheidung | Ohne Produktivdaten klären |
| --- | --- | --- |
| Wie weit trägt fsync über Docker/VM/VirtioFS bis zum physischen Speicher? | Erfolgreich angenommene Dateien bleiben nach echtem Strom-/Cache-Verlust erhalten | Isolierte VM mit gleichem Storage-Pfad und synthetischen Payloads; kontrollierte Power-Cut-Simulation, Storage-Cache-Konfiguration separat prüfen. |
| Wie reagiert konkrete Celery-/Redis-Kombination auf jeden Kill-Punkt? | Zahl der Zustellungen, ACK-Reihenfolge, Zustand nach Worker-/Brokerverlust | Exakte Images und Paketversionen in Teststack, instrumentierte ACK-/Publish-Barrieren. Keine Remote-Control-Publishes in Produktion für diesen Bericht. |
| Soll jede unterschiedliche Batch-ID trotz identischer Bytes ein eigenes Dokument erhalten? | Fachentscheidung über globale Inhaltsregel vs. Operationsidentität | Testfälle mit zwei synthetischen gleichen PDFs; Benutzerentscheidung vor Konfigurationsänderung. |
| Sind Split, Collate, Versionserzeugung und arbitrary Hooks für garantierte Importe erforderlich? | Einzeldokument-Vertrag oder atomar zugeordnete Kindoperationen | Synthetische Barcode-/Collate-Fixtures; zuerst enger Modus, Erweiterung separat entscheiden. |
| Wie lange müssen Schlüssel/Tombstones überleben? | Unbegrenzte Historie oder expliziter Horizont T; Datenschutzanforderungen | Speicherabschätzung mit Dummy-Operationen und Produktentscheidung, keine echten Dokumentmetadaten nötig. |
| Wie werden alte mehrdeutige Receipts übernommen? | Welche Belege reichen für sichere Zuordnung, was bleibt manuell? | Gefälschte Alt-Receipts mit eindeutigem, fehlendem und mehrfach belegtem Dateinamen; keine automatische Migration produktiver Dateien. |
| Welche externen Empfänger unterstützen Idempotenz wirklich? | Replay-Vertrag und dauerhafte Deduplizierung auch nach Empfänger-Restore | Dokumentationsprüfung plus lokale Mock-Empfänger; echte Anbieter nur in gesondert freigegebener Sandbox mit synthetischen Texten. |
| Wie funktionieren Fingerprint-Versionierung und geänderte Defaults? | Gleicher semantischer Request bleibt gleich trotz Reihenfolge; Profiländerung explizit | Golden-JSON-Fixtures und Versionswechsel im Teststack. |
| Reicht ein gemeinsamer Checkpoint oder braucht es ein nicht zurückgerolltes Journal? | Akzeptierter Datenverlust/RPO und Zusage über alte Bestätigungen | Restore-Drills mit ausschließlich synthetischer History und externem Mock-Effektregister. |
| Wie soll Upstream diesen Funktionsumfang anbieten? | Separate Import-API vs. ausgehandelter Upload-Modus; DB-Portabilität | Zuerst technischen Entwurf und Testnachweise diskutieren. Keine PRs/Issues oder Nachrichten wurden in diesem Auftrag veröffentlicht. |

**Abnahme dieses Berichts:** Der installierte Importpfad ist gegen die Basis abgeglichen; zentrale Aussagen sind mit Datei/Funktion/Zeilen oder ausdrücklichen Annahmen versehen. Die Matrix benennt die beobachtbaren Folgen aller geforderten Commit-Grenzen; E1–E8 beschreibt deren Behandlung und verbleibende Grenzen. Produktcode und laufende Dienste wurden nicht verändert. Eine funktionierende Implementierung oder praktische Exactly-once-Garantie wird damit ausdrücklich noch nicht behauptet.
