# B07: Scanner ohne Export-Unterbrechung sichern

Entwurf zur Betreiberfreigabe, 31.10.2026. **Kein Exportskript, Timer, produktiver
Dienst oder Snapshot-Mechanismus wird mit diesem Konzept geändert.** Die
Implementierung und ihr isolierter Restoretest benötigen einen Folgeauftrag.

## Empfehlung und Abnahmeziel

Empfohlen wird ein **von nx-boss selbst erzeugter Hardlink-Snapshot mit Manifest**,
abgerufen über einen ausschließlich lokal erreichbaren Unix-Socket. Der Exporter
liest danach nur die bereitgestellte Generation. nx-boss wird weder gestoppt noch
neu gestartet; laufende Uploads werden nicht abgebrochen, Scanner-Bearer bleiben
im selben Prozess gültig. Paperless-Webserver und Broker bleiben für ihren Export
vorerst wie bisher gestoppt. Reines Weglassen des nx-boss-Stopps reicht nicht.

Die Abnahme muss ohne nx-boss-Stop/Start, ohne VM-Freeze während aktiver Uploads,
mit unverändert gültigem Scanner-Token, kontrollierter Warteschlange und
manifestgeprüftem Restore gelingen. Ein eingeschränkter, ausgelassener oder
fehlgeschlagener nx-Teil macht die Gesamtgesundheit mindestens WARN/FAILED; ein
erfolgreicher Restic-Lauf allein darf das nicht überdecken.

## Lesend erhobener Ist-Zustand

| Beobachtung | Beleg / Grenze |
|---|---|
| Exportplan 02/06/10/14/18/22 Uhr, bis 15 Minuten Zufallsverzögerung | Homelab `restic-files-pve.conf` und `restic-files.timer`; Timer wurde nur gelesen. |
| Ein nx-boss-Stopp/Start beim heutigen 10-Uhr-Lauf: 10:01:04.005532 bis 10:07:56.275849 CET | Zwei Journald-Zeitstempel aus `restic-files.service`, reduziert auf die bekannte damalige nx-Container-ID. Abstand **412,270317 s, etwa 6 min 52 s**. Es sind Empfangszeiten der CLI-Meldungen, kein millisekundengenauer HTTP-Verfügbarkeitsmesser. |
| Zweiter regulärer Lauf direkt beobachtet | Docker `FinishedAt` 14:21:25.782757 bis `StartedAt` 14:32:11.134172 CET: **645,351415 s, etwa 10 min 45 s** zwischen Prozessende und Neustart. Read-only Polling alle zehn Sekunden bestätigte Export aktiv sowie App gestoppt/gestartet. HTTP-Bereitschaft kann zusätzlich später liegen. Keine manuellen Export- oder Restart-Aufrufe. |
| Ganze Backupjobs dauern erheblich länger | Journald enthält z.B. 30.10. 10:08:55.669311–13:50:51.004868 CET. Dieser Zeitraum enthält weitere Exporte/Transfer und ist **nicht** Scanner-Ausfallzeit. |
| Keine belastbare historische Scan-Überlappungsquote | Die alte App wurde beim Review-Rollout ersetzt; ihre Request-Logs sind hier nicht verfügbar. Docker lieferte im abgefragten zurückliegenden Fenster keine verwertbaren Stop/Start-Ereignisse. Der Review-Container lief zunächst seit 12:47:40.981267 CET; bis 13:24:29 CET gab es dort null Scan-Anfragen, aber keinen Export im selben Fenster. Beim später beobachteten 14-Uhr-Lauf ist eine historische Überlappungsquote ebenfalls nicht ableitbar. Daraus folgt keine Nullquote für frühere Exporte. |
| VM-Dateisystem ext4, kein Gast-LVM für diesen Pfad | `/opt/nx-boss-rs` liegt auf `/dev/sda2`, Mount `/`. |
| Proxmox-Storage unterstützt Snapshots | VM 100: `local-zfs:vm-100-disk-1`, 128 GiB; Storage-Typ `zfspool`, Guest Agent konfiguriert (`agent: 1`). Kein Snapshot oder Freeze ausgelöst. |

Die Ausfallzeiten sind **zwei konkret belegte Läufe mit unterschiedlichen
Messgrenzen**, keine repräsentative Verteilung über sechs Läufe pro Tag. Eine Tagesausfallzeit oder Zahl unterbrochener Scans wäre aus diesen
Daten erfunden. Der Folgeauftrag muss pro Generation Beginn/Ende, aktive Uploads,
aktuelle Scanner-Prozessinstanz und Manifeststatus als redigierte Metadaten
protokollieren; keine Tokens, Gerätenamen, Dateiinhalte oder Dokumentnamen.

## Was der heutige Stopp absichert

Quelle: vorhandenes `homelab/backup/restic/export-docker-apps.sh`, unverändert.
Die App wird zusammen mit anderen Schreibern vor Dumps und dem großen lokalen Tar
angehalten und erst danach gestartet. Broker wird nach den Schreibern gestoppt
und vor ihnen wieder gestartet. nx-boss steht damit auch während Fotos-/Paperless-
Kopien still; seine In-Memory-Scanner-Sitzungen gehen verloren.

| Bestandteil | Schreibverhalten im aktuellen nx-boss | Was ein Live-Tar ohne Koordination verlieren/vertauschen kann |
|---|---|---|
| Seiten | Neue JPEGs privat/atomar veröffentlicht; identische Retry-Datei bleibt unverändert, geänderter Retry wird abgelehnt | Metadaten können eine Seite nennen, deren Datei erst nach dem Tar-Durchlauf entsteht, oder Retention/Verwerfen entfernt die Seite während des Lesens. |
| `metadata.json` | Atomar ersetzt, nicht in-place erweitert | Ein gültiges JSON allein garantiert keine passende Menge von Seiten/PDF/Zustellbelegen im selben Archiv. |
| Staged PDF und Paperless-Beleg | PDF vor Abschluss gespeichert; Versuch/Task/Dokument-ID werden atomar fortgeschrieben | API kann schon angenommen haben, obwohl Snapshot den älteren Beleg enthält. Ein alter positiver Beleg kann außerdem zu einem anderen Paperless-Restorezeitpunkt gehören. |
| Konfiguration/Auth | Config, Passkeys und Trusted Devices privat/atomar; Pepper/Legacy-Datei werden gelesen | Unabhängig gelesene Dateien können verschiedene Konfigurationsgenerationen abbilden; alte Auth-Snapshots können Widerrufe zurücknehmen. |
| Retention | Archiv-Tempdatei wird in-place aufgebaut, danach publiziert; Sidecar atomar; Quelldir wird entfernt | Live-Tar kann Quelle verpassen und Archiv zu früh/unvollständig sehen. Temporäre `.discarded-*` und Archivdateien sind keine fertigen Sicherungen. |

Der neue Snapshot muss deshalb neben Upload/Completion auch Worker-Zustellungen,
Recovery/Verwerfen, Retention, Konfiguration und Auth-Schreibpfade koordinieren.
Der bestehende Delivery-Lock allein deckt nicht alle davon ab.

## Vergleich der Varianten

| Variante | Bewertung |
|---|---|
| Live-Tar + Manifestprüfung | Kein Stopp, aber ohne Änderungen an allen destruktiven Schreibern kein stabiler Mehrdateienstand. Nachprüfen kann eine Lücke erkennen, aber fehlende Seiten nicht rekonstruieren. Nicht allein empfehlen. |
| Hardlinks unter kurzer interner Sperre | Geeignete Datenmechanik: veröffentlichte Dateien bleiben bei atomarem Ersetzen über ihren alten Inode erhalten. Nur veröffentlichte reguläre Dateien, keine Temps/Symlinks; Retention/Verwerfen müssen die Generation respektieren. **Teil der Empfehlung.** |
| App-eigener Export über lokalen Weg | Kennt Jobs, Batchzustände und Sperren und kann ein vollständiges Manifest ausgeben. Die Sperren halten nur die Link-/Metadaten-Erfassung, niemals Kompression oder Übertragung. **Empfohlene Steuerung für den Hardlink-Snapshot.** |
| Proxmox-/ZFS-Snapshot | Host-ZFS ist vorhanden; fehlendes Gast-LVM verhindert einen Host-Snapshot nicht. Ein VM-Snapshot allein enthält VirtioFS-Paperless-Daten nicht konsistent mit dem Dump. Guest-Freeze blockiert Schreib-I/O und kann Uploads anhalten; deshalb nicht Primärlösung für die strenge Scanneranforderung. |
| Auf offene Batches warten | Nur begrenzter Rückfall bei nicht sicher erfassbarem Zustand. Kein endloses Warten und niemals danach den Scanner stoppen. Nach Obergrenze Generation als eingeschränkt/fehlgeschlagen markieren. |

Linux-Hardlinks funktionieren nicht über verschiedene Mounts, auch wenn dasselbe
Dateisystem mehrfach gemountet ist. Daher je eine Snapshot-Wurzel **innerhalb**
von `/data` und `/config`; kein Link aus beiden Bind-Mounts in einen dritten
`/backup`-Mount. Hardlinks teilen Inhalt, Eigentümer und Rechte: an der eingefrorenen
Generation niemals `chmod`, `chown` oder in-place schreiben. Die privaten
Verzeichnisse begrenzen den Zugriff; bestehende Dateirechte bleiben erhalten.
[Linux link(2)](https://man7.org/linux/man-pages/man2/link.2.html)

Proxmox beschreibt Freeze/Thaw bei Snapshot-Backups und weiteren Operationen.
Die Wirkung darf nicht mit „VM läuft weiter“ gleichgesetzt werden: eingefrorene
Schreibzugriffe warten. Die tatsächlich verwendete Freeze-Einstellung des
laufenden Backupwegs wurde nicht geändert oder praktisch ausgelöst. Ein bloßes
Abschalten des Freeze ersetzt keine Konsistenzstrategie. Die aktuelle offizielle
Doku nennt `freeze-fs`; ältere Anleitungen nennen `freeze-fs-on-backup`.
[Proxmox-Dokumentationsquelle](https://github.com/proxmox/pve-docs/blob/master/qm.adoc#qm_qga_fsfreeze)

## Vorgeschlagener Ablauf und Sperrvertrag

1. Exporter stoppt weiter die bislang freigegebenen anderen Schreiber und den
   Paperless-Broker, **nx-boss wird aus Stopp- und Restartliste entfernt**. Erst
   nach bestätigtem Paperless-Stillstand wird dessen Dump-/Daten-Grenze festgehalten.
2. Ein nur lokal erreichbarer App-Weg erzeugt eine neue eindeutige Generation.
   Unix-Socket und Snapshot-Verzeichnisse 0700/0600; Zugriff nur App-UID und
   root/Backup-Identität. Kein öffentlicher HTTP-Pfad, keine Shell-/Pfadparameter.
3. Jobinventar und Config/Auth-Generation erfassen; je Batch unter seiner Sperre
   nur die referenzierten publizierten Dateien verlinken. Worker-Belege über den
   Delivery-Lock erfassen. Retention/Verwerfen erhalten eine neue Snapshot-Lease,
   sodass sie während der Erfassung keine erfassten Originale entfernen.
   Einheitliche dokumentierte Lock-Reihenfolge; keine gegeneinander verschachtelten
   Map-/Batch-Sperren. Aktive Uploads werden weder abgebrochen noch abgeschlossen.
4. Der Snapshot darf einen **offenen Batch mit allen bis zum Erfassungszeitpunkt
   bestätigten Seiten** enthalten. Er bleibt im Manifest offen. Weitere Seiten
   und der laufende Scan gehen in der Live-Generation weiter. Noch nicht bestätigte
   Uploadbytes sind nicht Teil dieses Wiederherstellungspunkts. Neue/fehlende
   Metadaten während Erstellung führen zu erneutem Erfassen oder sichtbarem Fehler.
5. Manifest: Version, Run-ID, Commit, Erfassungsintervall, Job-/Batchinventar,
   Zustand, letzte Aktivität, Seiten/PDF/Archiv/Config-Bezüge, Größen und SHA-256,
   offene Batches, ausgelassene Einträge samt festem Fehlercode, Paperless-Dump-ID
   und dessen Zeitgrenze. Hashberechnung und Manifestserialisierung erst nach Freigabe der Schreibsperren auf den bereits verlinkten Dateien durchführen. Private Namen/Hashes bleiben im verschlüsselten Export,
   Statuslogs enthalten nur Zähler/Zeit/Fehlercode. Manifest zuletzt atomar und
   synchronisiert veröffentlichen, dann Generation unveränderlich behandeln.
6. Direkt nach Link-Erfassung sämtliche App-Sperren freigeben; Tar/Kompression und
   Netzwerktransfer laufen ausschließlich auf den eingefrorenen Generationen.
   Snapshot-Hashing und Archivierung nutzen eigene begrenzte Hintergrundressourcen; keine Scanner-Upload-/PDF-Slots belegen. Den großen Gast-Tar niedriger priorisieren und unter realistischer I/O-Last messen. nx-boss bleibt während des ganzen Exports erreichbar. Der große bestehende Tar
   muss die **Live-Nx-Pfade ausschließen** und stattdessen genau diese Generationen
   aufnehmen, sonst würde er wieder unstabile oder doppelte Daten mitsichern.
7. Auch `.env`, Pepper, aktive Token-Dateien und ggf. die noch aktive Legacy-
   Scannerdatei müssen im expliziten privaten Exportinventar bleiben. Unbekannte
   Konfigurationsdateien nicht still weglassen. ACME-/Proxyzustand separat prüfen;
   das App-API darf keine Secrets ins Netzwerk oder Statusjournal ausgeben.
8. Exporter prüft Manifest, Vollständigkeit, Hashes und Archiv vor Erfolgsmeldung.
   Abschluss/Abbruch gibt die Lease frei; verwaiste Generationen werden gezielt
   bereinigt. Keine automatische Rückkehr zum alten Stoppen bei Fehlern.

**Grenzen, vorgeschlagene Startwerte zur Freigabe:** Maximal fünf Minuten
Warte-/Retrybudget pro nx-Generation, einzelne Lock-Versuche nichtblockierend
mit kurzem Budget. Ziel <100 ms zusätzlicher Commit-Latenz, harte Abnahme ohne
Scanner-Requestabbruch; reale I/O-Latenzen erst im isolierten und physischen Test
belegen. Keine Sperre über eine ganze Scan-Dauer und kein Freeze als Fallback.
Nach Deadline Backupteil als fehlgeschlagen melden und Scanner weiterlaufen lassen.

Snapshot-Leases dürfen Retention nur begrenzt verzögern. Speicherreserve und
maximale gehaltene Generationen festlegen (Startvorschlag: 10 GiB Reserve,
höchstens zwei Generationen, überwachte Lease). Bei Speicherdruck zuerst den
Backupversuch abbrechen und seine Links freigeben; nie dafür Scanner-Anfragen
abbrechen. Schwellen müssen mit dem realen Scanvolumen und 128-GiB-Gastdisk geprüft
werden. Keine Garantie allein aus den vorgeschlagenen Zahlen ableiten.

## Restic-Health: keine stille Lücke

Die bestehende Health-Auswertung erhält im Folgeauftrag einen versionierten
nx-Teilstatus. Ohne passendes Manifest derselben Run-ID darf der Gesamtjob nicht
OK werden, auch wenn andere Anwendungen erfolgreich gesichert wurden.

| Zustand | Ergebnis |
|---|---|
| Manifest vollständig, geschlossenes Inventar, Hashprüfung und Restic-Snapshot bestätigt | OK mit Alter des letzten nx-Wiederherstellungspunkts |
| Offene Batches bewusst als offene Teilstände erfasst | WARN mit Anzahl/Erfassungszeit und Hinweis auf unvollständige Dokumente |
| Warten auf Sperren/Batchzustand innerhalb fünf Minuten | RUNNING; letzter erfolgreicher Zeitpunkt wird nicht vorgeschoben |
| Deadline, fehlende Seite/Config, Snapshot-/Socket-/Hash-/Platzfehler, fehlende Run-ID | FAILED/WARN nach definierter Schwere, niemals still überspringen |
| Alter letzter vollständiger nx-Stand länger als ein Planintervall plus Budget | Kritische Health-Meldung auch bei erfolgreichem restlichem Backup |

Retries verwenden denselben Run-Kontext oder weisen den Abbruch der alten
Generation aus. Das bestehende Export-Exitcode-/Trap-Verhalten muss den nx-Fehler
bis `restic-health` durchreichen; eine Warnung nur im SSH-Stderr reicht nicht.
Die anderen Anwendungen müssen bei Abbruch wieder starten. nx-boss bekommt weder
im normalen noch im Fehlerpfad einen `docker stop/start`-Aufruf.

## Paperless-Kopplung und belegter Nachlauf

Die Scanner-Anfragen speichern JPEG, Metadaten und PDF lokal. Der Paperless-
Worker läuft getrennt; ein nicht erreichbarer Webserver führt zu einem ausstehenden
Beleg, nicht zu einem Scanner-Neustart. Brokerstillstand verhindert Verarbeitung,
aber nicht die lokale Annahme. Ausstehende Batches bleiben von Retention geschützt.
Das ist aus den vorhandenen Schreib-/Workerpfaden abgeleitet und isoliert geprüft,
kein neu durchgeführter Test am produktiven Paperless.

**Ausgeführter isolierter API-Ausfalltest:** normales `userpass`, echte JPEGs,
vorhandenes Binary, temporäre Daten und eine simulierte Paperless-v10-API. Bei HTTP
503 wurden zwei Scans in 0,005 s und 0,003 s lokal abgeschlossen. Nach Rückkehr
der API bei 30,1 s waren beide Dokumente im ersten Lauf bei 180,1 s bestätigt.
Ein zweiter Lauf mit expliziter Prüfung auf genau zwei erfolgreiche Uploads war
bei 210,1 s fertig; derselbe Scanner-Bearer blieb gültig. Der reproduzierbare
Nachweis liegt in `scripts/test_paperless_outage.py` (Binary als Argument).
Die Verzögerung enthält den vorhandenen 30-s-Worker-Takt, dessen Rundung auf den
nächsten Tick und den 120-s-Abgleich vor erneutem Senden. Es gab keinen produktiven Upload, Paperless-
Restart oder Backup-Lauf. Ein Test-Double beweist nicht das Verhalten jeder echten
Paperless-Version oder eines physischen Scanners.

**Wiederherstellungsgrenze:** Manifest hält getrennt den Zeitpunkt/Identifier des
Paperless-Dumps samt Media/Broker und das nx-Erfassungsintervall fest. Nach dem
Stillstand von Paperless kann nx neue lokale Scans annehmen; diese liegen dann
hinter der Paperless-Grenze und bleiben als wartende Zustellungen erkennbar.
Ein gleichzeitiger globaler Zeitpunkt wird nicht vorgetäuscht.

Restore zuerst in isolierter Umgebung mit pausierter API-Zustellung:

- Jede bereits bestätigte nx-Dokument-ID gegen den wiederhergestellten Paperless-
  Stand und eine belastbare Batch-Referenz prüfen. Fehlende/falsche Zuordnung wird
  zum Klärungsfall, nicht automatisch als erfolgreich übernommen.
- Wartende/unklare Belege mit Task-ID und exakt passendem `<batch-id>.pdf` abgleichen.
  Task-Historie kann fehlen; dann reichen Dokument-ID oder gleicher Titel allein
  nicht als Dedup-Beweis. Permanent gespeicherte Batch-ID/Originalreferenz prüfen,
  sonst manuell klären. Kein automatisches blindes erneutes Senden.
- Erneutes Senden und manuelle ID-Zuordnung bleiben die vorhandenen bestätigten
  Operatoraktionen. Originalseiten bis zur Klärung behalten.
- Das Konzept erzeugt keine Exactly-once-Garantie. Für neue Batches wäre eine
  langfristig abfragbare Batch-ID-Referenz in Paperless zweckmäßig; deren konkrete
  Konfiguration/Backfill benötigt separate Betreiberfreigabe.

## Scanner-Sitzungen bei Export und Neustart

Kein Prozessneustart bedeutet, dass heutige In-Memory-Sitzungen den Export bereits
überleben können. Persistenz ist daher **keine Voraussetzung** des empfohlenen
Exports und keine Rechtfertigung für einen Stopp während eines Scans.

Persistente Scanner-Sitzungen könnten geplante Deployments/Neustarts verbessern:
256-Bit-Bearer, nur Hash und absolute Ablaufzeit privat/atomar speichern, maximale
Sitzungszahl und acht Stunden Gültigkeit erhalten. Rotation, Moduswechsel und
Logout müssen auf Platte widerrufen; beschädigte Datei fällt auf Neuanmeldung
zurück. Risiken: gestohlene Bearer bleiben auch nach Neustart gültig, alte Backups
können Sitzungen wiederbeleben, zusätzliche Schreib-/Widerrufskonsistenz und
Uhrabhängigkeit. Das ist ein gesondertes Feature. Im aktuellen Auftrag bleiben
Scanner-Sitzungen flüchtig; F2 betrifft nur Admin-Gerätevertrauen.

## Isolierter Restore- und Fehlertest vor Freigabe zur Produktion

Der vorgeschlagene Snapshot-Mechanismus wurde **noch nicht implementiert**. Folgender
Nachweis ist deshalb Abnahmearbeit des Folgeauftrags, kein behauptetes Ergebnis:

1. Wegwerf-Compose mit denselben UID/Rechten, getrennten Config-/Scans-Bind-Mounts,
   echten JPEGs, lokalem API-Double und `userpass` aufbauen. Keine produktiven
   Tokens, Paperless-Dokumente, Scanpfade oder Exportjobs verwenden.
2. Seite 1 bestätigen, Seite 2 langsam multipart hochladen, währenddessen Snapshot
   anfordern; danach Seite 3 und `PUT`. Keine 401/503/5xx oder Requestabbrüche
   durch den Export, keine Neuanmeldung, gemessene Zusatzlatenz im Budget. Offener
   Snapshot muss sämtliche zum Erfassungszeitpunkt bestätigten Seiten haben.
3. Snapshot langsam archivieren, während Live-Scan weitere Seiten schreibt. Alte
   verlinkte Metadaten/Dateien dürfen sich nicht ändern. Gleichzeitig Retention,
   Recovery, Tokenrotation, Passkey-/Gerätewiderruf und Journal testen.
4. Jeder Crashpunkt zwischen Seite/Metadaten, PDF/Abschluss, API/Beleg und
   Archiv/Sidecar/Quelllöschung muss entweder einen vollständigen deklarierbaren
   Stand oder einen sichtbaren Generationenfehler ergeben. Keine defekte Generation
   als OK markieren; keine in-place veränderlichen Temporärdateien aufnehmen.
5. Nur die Generation in ein leeres Restore-Verzeichnis übertragen. Manifest und
   Hashes prüfen; nx mit separatem Pepper/Config und zunächst deaktivierter
   Zustellung starten. Offene Scans offen anzeigen, vorhandene Seiten lesen und
   nach Neuanmeldung/Bestätigung sicher abschließen. Diese Neuanmeldung nach einem
   Restore ist von der während des Exports weiterlaufenden Originalsession getrennt.
6. Paperless-Dump/Media/Broker isoliert restaurieren. Die Fälle „API antwortete
   verloren“, „Dokument erst nach Dump entstanden“, „Task-Historie entfernt“ und
   „nx-Beleg bestätigt, Dokument fehlt“ prüfen. Kein automatisches Duplikat oder
   stilles Verwerfen; erst nach Abgleich Zustellung freigeben.
7. `EXDEV`, fehlende Rechte, Symlink, beschädigte Metadaten, fehlende Seite, fehlender
   Socket, voller Datenträger, Lease-Verlust, fehlender Run-ID, Abbruch während
   Tar und fünf Minuten aktive Scans simulieren. Health muss die Einschränkung
   zeigen, Scanner weiterarbeiten, andere Apps über ihren Trap wieder anlaufen.
8. Erst danach physischer fi-7300NX-Test während eines **separat freigegebenen**
   Exports und überwachte Beobachtung mehrerer regulärer Planläufe. Anforderung
   „laufenden Scan nie unterbrechen“ ist eine Freigabebedingung, keine Annahme.

## Umsetzung, Aufwand, Risiken und Rollback

Nötig wären App-Snapshot-/Lease-/Manifestcode mit lokalem Steuerweg, passende
Compose-Mount-/Socketrechte, Exporter-Umstellung auf Generationen statt Live-Nx-
Pfade sowie Health-/Restoreintegration. Grobe Schätzung nach Quellprüfung:
**4–7 Entwicklungstage** einschließlich Fehler-/Restorematrix, zusätzlich
Betreiberzeit für physischen Test und Beobachtung geplanter Läufe. Kein Termin-
oder Leistungsversprechen; Kosten der realen Dateianzahl/I/O erst messen.

Größte Risiken sind unbekannte in-place Schreiber, Lock-Reihenfolge und lange I/O,
über Mountgrenzen fehlgeschlagene Links, durch Leases belegter Speicher, unvollständige
Inventare und der Paperless-Zeitversatz. Nur nach isolierter Abnahme zuerst auf
Wegwerfdaten, dann nach Betreiberfreigabe am regulären Exportweg einführen.

Rollback darf **nicht heimlich den alten Scannerstopp zurückbringen**. Bei Problemen
Snapshotfeature abschalten, nx weiterlaufen lassen, nx-Backupteil sichtbar FAILED
melden und zum letzten überprüften Wiederherstellungspunkt zurückgreifen. Andere
Appbackups können getrennt weiterlaufen. Eine temporäre Ausnahme mit Scannerstopp
wäre eine neue ausdrückliche Betreiberentscheidung mit Dauer/Häufigkeit; sie ist
hier nicht empfohlen. Laufende Scan-/Auth-Daten beim Code-Rollback nicht zurückspielen.

## Entscheidungen für den Folgeauftrag

- Empfohlene App-gesteuerte Hardlink-Generation samt lokalem Socket freigeben?
- Fünf Minuten Retrybudget, WARN bei offen erfasstem Batch und kritische Meldung
  nach einem verpassten vollständigen Planintervall bestätigen?
- Speicherreserve, Anzahl/TTL/Heartbeat der Generationen anhand des Scanvolumens
  festlegen; bei Druck hat Scannen Vorrang und der Backupteil schlägt sichtbar fehl.
- Langfristige Paperless-Batch-Referenz für Restore-Abgleich separat konfigurieren?
- Persistente Scanner-Sitzungen als eigenes späteres Feature behandeln?

Die Backupunterbrechung ist mit diesem Konzept **noch nicht behoben**. Das vorhandene
Exportskript und die geplanten Läufe bleiben in diesem Auftrag unverändert.
