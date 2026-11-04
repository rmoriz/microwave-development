# Chef-Client-Timer auf den e2e-Nodes — Kurzdesign

## Intent

Die vier e2e-Nodes sollen nach dem initialen Converge alle 10 Minuten erneut
konvergieren, damit Änderungen, die über cinc-console oder MCP am Server
gemacht werden (Rolle, Umgebung, Node-Attribute), ohne manuellen Eingriff
ankommen. Der Timer ergänzt den bestehenden Boot-Converge und den
Path-Trigger; er ersetzt keinen davon.

## Goals

- `chef-client::systemd_service` aus dem Cookbook `chef-client` 12.3.4 wird
  Teil der Node-Rolle.
- `node['chef_client']['systemd']['timer'] = true`, `interval = 600`,
  `splay = 60`, `bin = /opt/cinc/bin/cinc-client`, `conf_dir = /etc/cinc`.
- `daemon_options` reichen `--json-attributes /etc/cinc/node.json`,
  `--log-level info`, `--no-color` an jeden Lauf weiter.
- Der Timer startet erst, wenn der initiale Converge vorbei ist.

## Non-Goals

- Kein Ersatz des Boot-Converges oder des `run-id`-Path-Triggers.
- Kein konfigurierbares Intervall über eine Host-Variable; 600 s ist fest.
- Keine Änderung an `auth`, Console oder MCP.

## Anti-Goals

- Ein Tick darf den initialen Converge nicht durch einen zweiten parallelen
  Client-Lauf stören (Paket-/Ressourcenkonflikt).
- Kein stiller Dauerfehler: ein fehlgeschlagener Tick bleibt im Journal
  sichtbar und der nächste versucht es erneut.

## Approach

Gewählt: **A mit Splay**, wie besprochen. Der Timer ruft `cinc-client` direkt;
die Registrierung passiert weiterhin einmalig über den Boot-Converge
beziehungsweise bei neuer Seed-Generation über den Path-Trigger.

Der erste Tick wird per Drop-in von `OnBootSec=1min` auf `5min` verschoben.
Das Cookbook erzeugt die Timer-Unit mit `OnBootSec=1min`; der erste
Client-Lauf würde sonst in den noch laufenden initialen Converge fallen.

## Änderungen

1. `docker-e2e/seed/seed.sh`: `chef-client` 12.3.4 in die Berksfile
   aufnehmen; `chef-client::systemd_service` hinter die beiden Wrapper in den
   `run_list` von `role.rb`; die Attribute aus Goals in `role.rb` setzen.
2. `docker-e2e/client/Dockerfile`: Drop-in
   `/etc/systemd/system/chef-client.timer.d/boot.conf` mit
   `[Timer]` / `OnBootSec=5min` installieren.
3. `docker-e2e/README.md`: Timer, Intervall, Splay und der Hinweis, dass
   Änderungen aus Console/MCP damit innerhalb von zehn Minuten ankommen.

Die Berksfile-Auflösung zieht `cron` und `logrotate` als Cookbook-Abhängigkeiten
mit; beide werden wie die anderen vendored Cookbooks hochgeladen.

## Verifikation

- `systemctl list-timers chef-client.timer` zeigt den nächsten Lauf; der Timer
  ist `active`.
- Nach dem initialen Converge und dem ersten Tick: zweiter Lauf im Journal,
  kein Ressourcenkonflikt.
- Eine Änderung über MCP (z. B. Node-Attribut) erscheint nach spätestens einem
  Intervall im nächsten Lauf.
- Ein absichtlich fehlschlagender Lauf (ungültiger Run-List-Eintrag) blockiert
  den Timer nicht; der nächste Tick läuft wieder.
- `docker compose config --quiet` und ein vollständiger Seed-Lauf bleiben grün.

## Offen für den Builder

- Genaue `daemon_options`-Reihenfolge und ob `--no-color` zusätzlich zur
  Journal-Ausgabe nötig ist.
- Ob der Drop-in-Pfad im Image vor dem ersten Start korrekt greift oder ein
  `systemctl daemon-reload` nötig ist (das Cookbook lädt nach dem Schreiben der
  Unit ohnehin neu).
