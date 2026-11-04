# Periodischer Chef-Client-Timer auf den e2e-Nodes — Implementierungsplan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Die e2e-Nodes nach dem initialen Converge fest alle 10 Minuten erneut konvergieren lassen, damit Änderungen aus cinc-console oder MCP ankommen.

**Architecture:** Ein kleines Site-Cookbook deklariert die in Cinc 19 eingebaute Ressource `chef_client_systemd_timer`. Sie legt eine Oneshot-Service- und eine Timer-Unit an, die den Node-Client mit dem gestagten `/etc/cinc/client.rb` und `--json-attributes` startet. Boot-Converge und `run-id`-Path-Trigger bleiben unverändert.

**Tech Stack:** Cinc Client 19.4.46, systemd, knife/Berkshelf im Seed, Chef-Ressource `chef_client_systemd_timer`.

**Spec:** `docs/superpowers/specs/2026-11-05-chef-client-timer-design.md`

## Global Constraints

- Basis ist `main`; nur `docker-e2e/` ändern, plus dieses Plan-Dokument.
- Festes Intervall: `interval "10min"`, `splay "1min"`, `delay_after_boot "10min"`.
- Kein `chef-client`-Cookbook, keine neuen Berkshelf-Abhängigkeiten.
- Client-Binary `/opt/cinc/bin/cinc-client`, Config-Verzeichnis `/etc/cinc`.
- Timer ergänzt `cinc-e2e-converge.service` und `cinc-e2e-converge.path`; beide bleiben unverändert.
- Der Timer läuft als `root` und nur auf den vier Node-Images.
- Keine Änderung an Console, MCP oder `auth`.

## Review Focus

1. Erster Tick kollidiert mit dem initialen Converge → `delay_after_boot "10min"`; Task 1 prüft die Unit-Werte und das Journal.
2. `daemon_options` landen im generierten `ExecStart` vor `-c` → Task 1 prüft die Unit und einen echten Lauf.
3. Ein fehlgeschlagener Lauf deaktiviert den Timer nicht → Task 2 provoziert einen Fehlschlag und prüft den nächsten Tick.
4. Der Timer läuft nach einer neuen Seed-Generation weiter und die Re-Registrierung greift → Task 2 wiederholt `up`.
5. Eine Änderung über MCP erreicht den Node innerhalb eines Intervalls → Task 2 setzt ein Marker-Attribut über MCP und beobachtet die Datei.

---

### Task 1: Timer-Cookbook und Rollen-Einbindung

**Files:**
- Create: `docker-e2e/seed/site-cookbooks/e2e_timer/metadata.rb`
- Create: `docker-e2e/seed/site-cookbooks/e2e_timer/recipes/default.rb`
- Create: `docker-e2e/seed/site-cookbooks/e2e_marker/metadata.rb`
- Create: `docker-e2e/seed/site-cookbooks/e2e_marker/recipes/default.rb`
- Modify: `docker-e2e/seed/seed.sh` (Upload-Schleife und `role.rb`)
- Modify: `docker-e2e/README.md` (Seed-Schritte)

**Interfaces:**
- Consumes: `chef_client_systemd_timer` aus Cinc 19; das gestagte `/etc/cinc/client.rb` und `/etc/cinc/node.json` aus `client/converge.sh`.
- Produces: Timer-Units `chef-client.timer`/`chef-client.service`, aktiviert beim initialen Converge; Node-Attribut `node['e2e_marker']` schreibt `/etc/e2e-marker`.

- [ ] **Step 1: `e2e_timer`-Metadata anlegen**

```ruby
name 'e2e_timer'
maintainer 'Microwave E2E'
license 'Apache-2.0'
description 'Runs Cinc Client every ten minutes on e2e nodes'
version '1.0.0'
chef_version '>= 16.0'
```

- [ ] **Step 2: `e2e_timer`-Rezept anlegen**

```ruby
chef_client_systemd_timer 'chef-client' do
  interval '10min'
  splay '1min'
  delay_after_boot '10min'
  chef_binary_path '/opt/cinc/bin/cinc-client'
  config_directory '/etc/cinc'
  daemon_options %w(
    --json-attributes /etc/cinc/node.json
    --log-level info
    --no-color
  )
end
```

- [ ] **Step 3: `e2e_marker`-Cookbook anlegen**

`metadata.rb` wie oben mit `name 'e2e_marker'` und Beschreibung
`Writes a marker file from a node attribute for timer verification`;
`recipes/default.rb`:

```ruby
file '/etc/e2e-marker' do
  content node['e2e_marker'].to_s
  mode '0644'
end
```

- [ ] **Step 4: Beide Cookbooks in die Upload-Schleife aufnehmen**

In `seed.sh` die Schleife zu
`for wrapper in e2e_nginx e2e_postgresql e2e_timer e2e_marker; do` ändern.

- [ ] **Step 5: Rollen-Run-List erweitern**

In `role.rb`:

```ruby
run_list "recipe[$NGINX_RECIPE]", "recipe[$POSTGRES_RECIPE]", "recipe[e2e_timer]", "recipe[e2e_marker]"
```

- [ ] **Step 6: README-Seed-Schritte ergänzen**

In „What the seed does“ nach dem Rollen-Schritt einfügen, dass die Rolle
zusätzlich den periodischen Client-Timer und den Marker installiert; die
Nummerierung bleibt sonst unverändert.

- [ ] **Step 7: Stack neu bauen und seeden**

```sh
cd docker-e2e
docker compose build node-debian node-ubuntu node-fedora node-amazon
docker compose up -d
docker compose wait seed
```

- [ ] **Step 8: Units und ersten Tick prüfen**

```sh
docker compose exec -T node-debian systemctl cat chef-client.timer
docker compose exec -T node-debian systemctl is-active chef-client.timer
```

Erwartet: `OnBootSec=10min`, `OnUnitActiveSec=10min`,
`RandomizedDelaySec=1min`, `active`. `systemctl cat chef-client.service`
zeigt `ExecStart=/opt/cinc/bin/cinc-client --json-attributes
/etc/cinc/node.json --log-level info --no-color -c /etc/cinc/client.rb`.

Nach zehn Minuten:

```sh
docker compose exec -T node-debian journalctl -u chef-client.service --no-pager | tail -20
docker compose exec -T node-debian cat /etc/e2e-marker
```

Erwartet: ein zweiter Lauf ohne Paket-/Ressourcenkonflikt; `systemctl
list-timers chef-client.timer` zeigt den nächsten Lauf.

- [ ] **Step 9: Commit**

```sh
git add docker-e2e/seed/site-cookbooks/e2e_timer docker-e2e/seed/site-cookbooks/e2e_marker docker-e2e/seed/seed.sh docker-e2e/README.md
git commit -m "test(e2e): converge the nodes every ten minutes"
```

---

### Task 2: Timer-Verhalten unter Last und Änderungen

**Files:**
- Modify: keine (nur Verifikation; Fixes am Code aus Task 1, falls nötig)

**Interfaces:**
- Consumes: aktiver Timer und Marker-Cookbook aus Task 1.
- Produces: belegtes Verhalten für Fehlschlag, Re-Seed und MCP-Änderung.

- [ ] **Step 1: MCP-Änderung setzen**

Im Workstation-Container, Token liegt als `MW_MCP_TOKEN`:

```sh
docker compose exec -T workstation bash -c '
  mcp() { curl -sS http://microwave:8000/organizations/e2e/mcp \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "Authorization: Bearer $MW_MCP_TOKEN" -d "$1"; }
  doc=$(mcp "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"get_node\",\"arguments\":{\"name\":\"node-debian\"}}}" | jq -r ".result.content[0].text")
  new=$(jq --arg v before ".normal.e2e_marker=\$v" <<<"$doc")
  mcp "$(jq -n --argjson n "$new" "{jsonrpc:\"2.0\",id:2,method:\"tools/call\",params:{name:\"update_node\",arguments:{name:\"node-debian\",node:\$n}}}")"
'
```

- [ ] **Step 2: Nächsten Tick abwarten und Datei prüfen**

Nach höchstens 11 Minuten:

```sh
docker compose exec -T node-debian cat /etc/e2e-marker
```

Erwartet: `before`.

- [ ] **Step 3: Fehlschlag provozieren**

```sh
docker compose exec -T workstation knife node run_list add node-debian 'recipe[does_not_exist]' --config /root/work/.chef/knife.rb
```

- [ ] **Step 4: Fehlschlag und Fortbestand des Timers prüfen**

Nach dem nächsten Tick:

```sh
docker compose exec -T node-debian journalctl -u chef-client.service --no-pager | tail -20
docker compose exec -T node-debian systemctl is-active chef-client.timer
```

Erwartet: Lauf endet mit Cookbook-Fehler; Timer bleibt `active`; der nächste
Lauf startet trotzdem.

- [ ] **Step 5: Fehlschlag zurücknehmen**

```sh
docker compose exec -T workstation knife node run_list remove node-debian 'recipe[does_not_exist]' --config /root/work/.chef/knife.rb
```

- [ ] **Step 6: Re-Seed bei aktivem Timer prüfen**

`docker compose up -d` erneut ausführen und `docker compose wait seed`
abwarten; danach prüfen, dass die Nodes neu registriert sind und der nächste
Tick wieder erfolgreich läuft (`latest_run`-Status oder Journal). Erwartet:
kein dauerhaftes 401 durch den gelöschten Client-Key.

- [ ] **Step 7: Commit, falls Fixes nötig waren**

Nur bei Änderungen; Nachricht `test(e2e): harden the converge timer`.

---

### Task 3: Dokumentation

**Files:**
- Modify: `docker-e2e/README.md`

**Interfaces:**
- Consumes: Verhalten aus Tasks 1-2.

- [ ] **Step 1: Timer-Abschnitt ergänzen**

Unter „Re-converging“ beschreiben: `chef-client.timer` konvergiert alle zehn
Minuten mit 1 Minute Splay, erster Lauf zehn Minuten nach dem Boot; der
initiale Converge und der Path-Trigger bleiben. Änderungen an Rolle,
Umgebung und Node-Attributen aus Console/MCP kommen damit innerhalb eines
Intervalls an. Ein fehlgeschlagener Lauf lässt den Timer aktiv.

- [ ] **Step 2: Prüfen**

```sh
cd docker-e2e
grep -n 'chef-client.timer\|zehn Minuten\|innerhalb eines Intervalls' README.md
docker compose config --quiet
```

- [ ] **Step 3: Commit**

```sh
git add docker-e2e/README.md
git commit -m "docs(e2e): describe the periodic converge timer"
```
