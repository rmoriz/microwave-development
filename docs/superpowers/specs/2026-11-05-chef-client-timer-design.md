# Chef-Client-Timer auf den e2e-Nodes — Kurzdesign

## Intent

Die vier e2e-Nodes sollen nach dem initialen Converge alle 10 Minuten erneut
konvergieren, damit Änderungen, die über cinc-console oder MCP am Server
gemacht werden (Rolle, Umgebung, Node-Attribute), ohne manuellen Eingriff
ankommen. Der Timer ergänzt den bestehenden Boot-Converge und den
Path-Trigger; er ersetzt keinen davon.

Das Cookbook `chef-client` ist deprecated. Genutzt wird die in Cinc 19
eingebaute Ressource `chef_client_systemd_timer` (in
`chef-19.4.46/lib/chef/resource/chef_client_systemd_timer.rb` vorhanden), die
genau dieses Muster abbildet. Damit entfallen Cookbook-Abhängigkeiten wie
`cron` und `logrotate`.

## Goals

- Eigene Site-Cookbook `e2e_timer` deklariert
  `chef_client_systemd_timer` in ihrem Default-Rezept.
- `interval "10min"`, `splay "1min"`, `delay_after_boot "10min"`,
  `chef_binary_path "/opt/cinc/bin/cinc-client"`,
  `config_directory "/etc/cinc"`.
- `daemon_options` reichen `--json-attributes /etc/cinc/node.json`,
  `--log-level info`, `--no-color` an jeden Lauf weiter.
- Der Timer startet erst, wenn der initiale Converge vorbei ist.

## Non-Goals

- Kein Ersatz des Boot-Converges oder des `run-id`-Path-Triggers.
- Kein konfigurierbares Intervall über eine Host-Variable; 10 Minuten sind fest.
- Keine Änderung an `auth`, Console oder MCP.

## Anti-Goals

- Ein Tick darf den initialen Converge nicht durch einen zweiten parallelen
  Client-Lauf stören (Paket-/Ressourcenkonflikt).
- Kein stiller Dauerfehler: ein fehlgeschlagener Tick bleibt im Journal
  sichtbar und der nächste versucht es erneut.

## Approach

Gewählt: **A mit Splay**, wie besprochen, aber mit der eingebauten Ressource
statt des Cookbooks. Der Timer ruft `cinc-client` direkt; die Registrierung
passiert weiterhin einmalig über den Boot-Converge beziehungsweise bei neuer
Seed-Generation über den Path-Trigger.

`delay_after_boot "10min"` verschiebt den ersten Lauf, damit er nicht in den
noch laufenden initialen Converge fällt. Die Ressource setzt den Timer auf
`OnBootSec=10min` + `OnUnitActiveSec=10min` + `RandomizedDelaySec=1min`.

## Änderungen

1. Neues Site-Cookbook `docker-e2e/seed/site-cookbooks/e2e_timer` mit
   `metadata.rb` und `recipes/default.rb` (die Ressource aus Approach).
2. `docker-e2e/seed/seed.sh`: `recipe[e2e_timer]` hinter die beiden Wrapper in
   den `run_list` von `role.rb`. Der bestehende Upload-Loop lädt das neue
   Cookbook mit.
3. `docker-e2e/README.md`: Timer, Intervall, Splay und der Hinweis, dass
   Änderungen aus Console/MCP damit innerhalb von zehn Minuten ankommen.

## Verifikation

- `systemctl list-timers cinc-client.timer` zeigt den nächsten Lauf; der Timer
  ist `active`.
- Nach dem initialen Converge und dem ersten Tick: zweiter Lauf im Journal,
  kein Ressourcenkonflikt.
- Eine Änderung über MCP (z. B. Node-Attribut) erscheint nach spätestens einem
  Intervall im nächsten Lauf.
- Ein absichtlich fehlschlagender Lauf blockiert den Timer nicht; der nächste
  Tick läuft wieder.
- `docker compose config --quiet` und ein vollständiger Seed-Lauf bleiben grün.

## Offen für den Builder

- Ob `daemon_options` vor oder nach `-c` im generierten `ExecStart` stehen
  (die Ressource hängt `-c` hinten an; beides muss von Cinc akzeptiert werden).
- Ob der erste Tick nach 5 Minuten in der Praxis kollisionsfrei ist oder
  `delay_after_boot` reicht.
