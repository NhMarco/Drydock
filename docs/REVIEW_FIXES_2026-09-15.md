# Umsetzung des Repository-Reviews — 15.09.2026

Die Korrekturen beziehen sich auf [docs/REPOSITORY_REVIEW_2026-09-15.md](C:/Users/Administrator/Documents/GitHub/Drydock/docs/REPOSITORY_REVIEW_2026-09-15.md). Der ursprüngliche Bericht bleibt als Befund des damaligen Stands erhalten; seine Zeilennummern beziehen sich auf diesen Stand.

## Ergebnis

Die bestätigten funktionalen Befunde wurden korrigiert. Die Architekturänderung bleibt gezielt: Downloadprotokoll, Pfadauflösung und Workerablauf liegen jetzt in einem eigenen Modul; der fachliche Abschluss erzeugt eine gemeinsame Settingsänderung. Die übrige UI wird nicht vollständig neu aufgebaut.

| Befund | Umsetzung |
| --- | --- |
| F01 | UTF-8-sicherer Suffixcheck; Regressionstest mit chinesischen Zeichen, Emoji und Großschreibung. |
| F02 | Große Limiteranforderungen werden in Teilbudgets erfüllt; Abbruch wird während des Wartens geprüft. Tests für große Anforderungen und Cancellation. |
| F03 | Installationssegmente und EXE-Pfade werden vor Verwendung validiert. Traversal, Laufwerke, UNC, Steuerzeichen und Windows-Gerätenamen werden abgelehnt. |
| F04 | Vorhandene Reparse Points werden abgelehnt. Depot-, Magicfiles- und transaktionale Schreibzugriffe arbeiten zusätzlich relativ zu einem fest geöffneten Stammverzeichnis über `cap-std`; spätere Unterpfadauflösung bleibt auf diesen Stamm beschränkt. |
| F05 | Serviceinstallation und -entfernung verwenden einen gemeinsamen Dateitransaktionsbaustein. Vollständige temporäre Kopie vor Rename; Rollbackfehler werden gemeldet und Originale samt Wiederherstellungszuordnung behalten. |
| F06 | Fix-ZIP wird vollständig in ein temporäres Verzeichnis entpackt, bevor Live-Dateien verändert werden. Overlay ist rücksetzbar, Lua wird zuletzt übernommen. |
| F07 | Payloads werden als neue Generation vorbereitet und anschließend getauscht. Vorherige Generation bleibt als `.bak` erhalten; Recovery liest sie bei unterbrochenem Tausch. In-process Zugriffe sind serialisiert. Speicherfehler erreichen Status/Fehlerpfad. |
| F08 | Aktivierungsbedingte Settingsänderungen verwenden den bestehenden zentralen Schreibschutz. |
| F09 | Pfadauflösung berücksichtigt registrierte Drydock-Installationen; Steam-Manifeste behalten Vorrang. Test für nachträglich geänderten Standardordner. |
| F10 | Der Worker liefert seinen tatsächlichen Zielpfad. Queueentfernung und Bibliothekseintrag werden gemeinsam gespeichert. Bei Speicherfehler bleibt die Queue erhalten und pausiert. EXE-Erkennung kann mehrere Ergebnisse unabhängig nachliefern. |
| F11 | Aktive temporäre Dateien werden vom Sweep ausgeschlossen. Cacheveröffentlichung und Sweep sind serialisiert; Sidecars erhalten eindeutige temporäre Namen. |
| F12 | `AbortSignal.timeout` bleibt auch beim Bodyverbrauch wirksam. Ein lokaler HTTP-Test liefert Header sofort und prüft den Abbruch des unvollständigen Bodys. |
| F13 | Ungültige Providerstrukturen und unbrauchbare leere Aggregationen erzeugen Fehler. Der letzte gültige Katalog bleibt bestehen. |
| F14 | Platzbedarf berechnet zusätzliches Dateiwachstum statt erneut die gesamte vorhandene Installation. Tests für vorhandene und zu kurze Dateien. |
| F15 | Verify prüft Dateiexistenz und exakte Größe unabhängig von Chunks; `bad_files` beeinflusst Gesamtstatus und UI-Fehlermeldung. |
| F16 | Eigener HTTP-Texturloader gibt nicht mehr sichtbare URIs frei; dekodierte Daten werden nach Texturupload freigegeben. Zurückgestellte Bereinigung beseitigt auch verlassene Ladeeinträge außerhalb der Loaderlocks. Headlesstest zeigt Freigabe von 100 Texturen. |
| F17 | Dateihandles werden auf höchstens 16 je Worker begrenzt, also höchstens 512 bei maximaler Parallelität. |
| F18 | Rust-Mindestversion und README auf 1.95 angehoben und lokal mit Rust 1.95.0 geprüft; eigener CI-Job ergänzt. |
| F19 | Dockerstufen auf Node 24 aktualisiert; reproduzierbare Installation mit `npm ci`; Nodeanforderung im Paket angeglichen. |
| F20 | `sevenz-rust` durch `sevenz-rust2` 0.22.2 ersetzt. Feste Extraktionsziele bleiben erhalten. Tests für Solid-Archive, beide Architekturen, optionalen Sound und beschädigte Archive. |
| F21 | Booleanparser akzeptiert ausschließlich bekannte Wahr-/Falschwerte; Tippfehler brechen den Start ab. |
| F22 | Neue Rust- und Proxyregressionstests; gemeinsamer HMAC-Vektor, Querybindung, Zeitfenster, Rotation und Replay getestet. Release hängt jetzt von Rust-/Proxyvalidierung ab. Normale Pushes/PRs lösen weiterhin keine zusätzlichen Workflows aus. |
| F23 | Downloadtypen, Pfadauflösung und Netzwerkworker aus `ui.rs` nach `downloads.rs` verschoben; dauerhafter Queue-/Bibliotheksabschluss im Core. Das UI-Modul bleibt groß, der konkret fehleranfällige Verantwortungsbereich ist jedoch getrennt. |
| F24 | Unbenutzte Produktionspakete `jszip` und `node-unrar-js` einschließlich Lockfileeinträgen entfernt. |
| R01 | Ohne GitHub-Token wird kein Authorization-Header gesendet; mit kontrolliertem Fetch getestet. |
| R02 | Lokalen Aufruf und vorhandene Signaturprüfung vor Entschlüsselung geprüft; RSA-Entschlüsselung verwendet zusätzlich Blinding. **Kein vollständiger Advisory-Fix behauptet**, siehe Grenzen. |

## Zentrale Dateien

- [crates/drydock-core/src/safe_path.rs](C:/Users/Administrator/Documents/GitHub/Drydock/crates/drydock-core/src/safe_path.rs) — Pfadvalidierung und auf ein Stammverzeichnis beschränkte Dateioperationen.
- [crates/drydock-core/src/file_transaction.rs](C:/Users/Administrator/Documents/GitHub/Drydock/crates/drydock-core/src/file_transaction.rs) — gemeinsamer Dateiersatz und Wiederherstellung.
- [crates/drydock-core/src/app_payloads.rs](C:/Users/Administrator/Documents/GitHub/Drydock/crates/drydock-core/src/app_payloads.rs) — Generationstausch und Payload-Recovery.
- [crates/drydock-core/src/depot/download.rs](C:/Users/Administrator/Documents/GitHub/Drydock/crates/drydock-core/src/depot/download.rs) — Limiter, Dateigrößen, Resume und Handles.
- [apps/drydock-desktop/src/downloads.rs](C:/Users/Administrator/Documents/GitHub/Drydock/apps/drydock-desktop/src/downloads.rs) und [crates/drydock-core/src/download_queue.rs](C:/Users/Administrator/Documents/GitHub/Drydock/crates/drydock-core/src/download_queue.rs) — Downloadablauf und dauerhafter Abschluss.
- [apps/drydock-desktop/src/image_cache.rs](C:/Users/Administrator/Documents/GitHub/Drydock/apps/drydock-desktop/src/image_cache.rs) — begrenzte Texturlebensdauer.
- [proxy/src/fileCache.ts](C:/Users/Administrator/Documents/GitHub/Drydock/proxy/src/fileCache.ts) — koordinierter Sweep und Veröffentlichung.
- [crates/drydock-core/src/review_regressions.rs](C:/Users/Administrator/Documents/GitHub/Drydock/crates/drydock-core/src/review_regressions.rs) und [proxy/src/review.test.ts](C:/Users/Administrator/Documents/GitHub/Drydock/proxy/src/review.test.ts) — übernommene Fehlerreproduktionen.
- [.github/workflows/ci.yml](C:/Users/Administrator/Documents/GitHub/Drydock/.github/workflows/ci.yml) und [.github/workflows/release.yml](C:/Users/Administrator/Documents/GitHub/Drydock/.github/workflows/release.yml) — Qualitätsvoraussetzungen für Releases. Der zuvor vorhandene Herkunftsguard bleibt erhalten.

## Verifikation

- Rust: **193 Coretests + 23 Desktoptests bestanden**, insgesamt **216**; ein bestehender netzabhängiger Test ignoriert.
- `cargo fmt --all --check` und Clippy für Workspace/alle Targets mit `-D warnings`.
- `cargo +1.95.0 check --workspace --locked --offline`: bestanden, separat von der aktuellen Standardtoolchain gebaut.
- Proxy: TypeScript-Build und **13 Tests bestanden**.
- `npm audit --omit=dev`: **0 gemeldete bekannte Produktionslücken** zum Prüfzeitpunkt.
- Vorhandene Aktivierungstests prüfen weiterhin Format- und Entschlüsselungskompatibilität nach Hinzufügen von Blinding.
- Kein Deployment, kein Push und keine Veröffentlichung durchgeführt.

## Grenzen und betriebliche Hinweise

- **RSA:** Das aktuelle offizielle Advisory meldet weiterhin keinen gepatchten Release. Blinding ist zusätzliche Härtung und ersetzt keine vollständige Korrektur dieses Upstream-Problems. Im untersuchten lokalen Aktivierungsablauf wurde kein öffentlich erreichbares Timingorakel nachgewiesen. [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).
- **Docker/andere Plattformen:** Auf diesem Rechner ist Docker nicht installiert. Der Dockerbuild ist im CI ergänzt, wurde hier aber nicht ausgeführt. Linux-/ARM- und tatsächliche GitHub-Actions-Läufe bleiben remote zu bestätigen.
- **Wiederherstellung:** Bei gescheitertem Rollback enthält das gemeldete Verzeichnis `drydock-recovery-*` (bei Denuvo-Fixes `.drydock-recovery-*` im Spielordner) die Originaldateien und `targets.jsonl` (eine JSON-Zeile je Ziel). Es wird dann absichtlich nicht automatisch gelöscht. Ein Prozessabbruch führt nicht zu einer automatischen Wiederaufnahme der gesamten Dateitransaktion; erhaltene Daten ermöglichen die Wiederherstellung.
- **Speicher:** Der Texturlebenszyklus ist per Headlesstest überprüft; eine reale GPU-/RSS-Langzeitmessung ist nicht durchgeführt worden.
- **Architektur:** Es handelt sich um die gezielte Extraktion des Downloadbereichs, nicht um die vollständige Zerlegung des restlichen UI-Moduls.

