# Request limits: Schutz vor rogue Clients zwischen Orgs und Principals

## Intent

Ein Server betreibt mehrere Organisationen. Ein einzelner fehlerhafter oder
bösartiger Client darf weder seine eigene Org noch andere Orgs lahmlegen.
Schutz nötig ist an zwei Achsen: **global** (der Prozess, sein DB-Pool, sein
RSS) und **innerhalb einer Org** (ein Node-Key gegen die übrigen Principals
derselben Org).

Die erste Ausbaustufe läuft **in-memory pro Prozess**. Es kommen keine neuen
Crate-Abhängigkeiten hinzu. Bei mehreren Replicas gilt jedes Limit pro
Replica; die Doku sagt das ausdrücklich.

## Goals

- Ein globales Admission-Limit für gleichzeitig bearbeitete API-Requests,
  unterhalb der DB-Pool- und Worker-Kapazität, mit begrenzter Wartezeit.
- Ein billiges Pre-Auth-Bucket pro Quell-IP, damit unauthentifizierter Müll
  nicht erst Org- und Principal-Lookups in der DB auslöst.
- Ein orgweites Request- und In-Flight-Limit.
- Ein engeres Request- und In-Flight-Limit pro Principal innerhalb der Org.
- Bookshelf-Uploads und -Downloads zählen gegen die orgweite Grenze wie
  API-Requests, weil sie bis zu 250 MiB RAM pro Request binden.
- `503` mit `Retry-After`, damit Cinc automatisch erneut versucht.
- Metriken und strukturierte Logs pro Scope, Org und Principal.
- Konfigurierbare Quoten mit per-Org-Overrides und einem globalen
  Ausschalter.
- Feste Obergrenzen für die Bucket-Maps, damit der Limiter nicht selbst
  unbegrenzt Speicher frisst.

## Non-Goals

- Kein geteilter Zustand über Replicas (kein Redis, kein Postgres-Counter).
- Keine gewichtete Fairness oder Priorisierung zwischen Orgs; jede Org hat
  denselben statischen Sockel.
- Keine Abrechnung, persistente Quoten oder Monatsbudgets.
- Kein Schutz vor einem Angreifer, der beliebig viele verschiedene Client-Keys
  in vielen Orgs kontrolliert; dann greift nur noch das globale Limit.
- Keine Änderung an der Signatur- oder Token-Authentifizierung selbst.
- Keine Begrenzung von Antwortgrößen (bestehende `max_result_bytes` bleiben).

## Anti-Goals

- Kein `429` als einzige Antwort: Cinc wiederholt nur `50x`, nicht `429`.
- Kein Limit für `/_status`, `/_stats`, `/version`, `/server_api_version`,
  `/license`, die Landing-Page und ihre Assets.
- Kein Body-Lesen vor dem globalen Gate.
- Kein Verhalten, das bei `limits.enabled = false` auch nur einen Request
  zusätzlich kostet.

## Approaches

**A — nur Token Buckets** pro Org und Principal. Glättet die Rate und begrenzt
Bursts, aber wenige langsame Requests können weiter den Pool halten.

**B — nur In-Flight-Limits.** Schützt Pool und RAM direkt, begrenzt aber keine
schnellen billigen Requests.

**C — Hybrid, gewählt.** Globales Admission-Semaphore, darunter ein
reservierter Sockel pro Org und ein engeres Limit pro Principal; derselbe
Mechanismus gilt für Bookshelf-Uploads. Statische Quoten, per Config
justierbar. Ein orgweites In-Flight-Limit ist die Isolationsgrenze, das
globale Limit die Decke. Ein einzelner rogue Client kann damit seine
Org-Quote ausschöpfen, aber nicht die Quote anderer Orgs.

## Design

### Reihenfolge der Layer

Von außen nach innen:

1. **Globale Aufnahme** (`limits::global_gate`, eigener Layer außen):
   exempt-Pfade durchreichen, sonst ein globales Semaphore-Permit mit
   `queue_timeout_ms` erwerben. Timeout → `503` + `Retry-After`. Der Body ist
   zu diesem Zeitpunkt noch ungelesen.
2. **Quell-Bucket** (`limits::source`): Token Bucket pro Quell-IP, bevor Auth
   DB-Lookups macht.
3. **Authentifizierung** wie heute.
4. **Org- und Principal-Bucket:** nach `AuthContext`, orgweit über `org.id`,
   principalweit über `(org.id, principal.actor_id)`. Pfade ohne Org
   (`/users`, `/organizations`, `/authenticate_user`, OAuth) bekommen nur
   globalen und Quell-Schutz; die Org-Quote gilt auch für `/validate`, nicht
   nur für Mutationen.
5. **MCP** ruft den Org-/Principal-Check nach der Bearer-Auth auf.
   **Bookshelf** ruft den Org-Check nach der Token-Prüfung auf und hat keinen
   Principal; der Check liegt als Router-Middleware vor dem `Bytes`-Extraktor,
   damit das Permit vor dem Puffern der bis zu 250 MiB greift. Downloads
   zählen ebenfalls gegen die Org-In-Flight-Grenze, weil auch sie puffern.

Der Org-Bucket läuft bewusst nach der Auth: Vor-Auth ist der Org-Name aus dem
Pfad nicht verlässlich (Default-Org-Rewrite, Bookshelf trägt die Org-ID), und
der DB-Lookup wird bereits vom Quell- und Global-Limit geschützt. Der
Org-Bucket ist mit `org.id` geschlüsselt; `limits.organizations.<name>` wird
gegen den aufgelösten `org.name` gematcht. Die Org-ID, nicht der Name, hält
Bookshelf und API im selben Bucket.

Permits werden bis zum Abschluss des Handlers gehalten; bei gepufferten
Antworten und Uploads endet das Halten mit dem Handler.

### Quoten

- Org und Principal: Request-Rate (Token Bucket, kontinuierliche Füllung) und
  In-Flight-Semaphore.
- Rate-Verstoß → sofort `503`.
- In-Flight-Verstoß → bis `queue_timeout_ms` warten, dann `503`.
- Global: nur In-Flight, kein Rate-Bucket; globale Rate wäre bei einem
  Multi-Org-Server der falsche Hebel.
- Der Org-Sockel ist statisch für alle Orgs gleich; per-Org-Overrides heben
  oder senken ihn.

### Standardwerte

| Schlüssel | Default | Bedeutung |
|---|---|---|
| `limits.enabled` | `true` | Schalter für die ganze Funktion |
| `limits.queue_timeout_ms` | `1500` | Maximale Wartezeit auf ein In-Flight-Permit |
| `limits.max_entries` | `10000` | Obergrenze je Bucket-Map |
| `limits.global.in_flight` | `32` | Gleichzeitige Requests pro Prozess; kleiner Multiplikator der Pool-/Worker-Kapazität |
| `limits.source.rate_per_second` | `500` | Requests/s pro Quell-IP |
| `limits.source.burst` | `1000` | Burst pro Quell-IP |
| `limits.org.rate_per_second` | `200` | Requests/s pro Org |
| `limits.org.burst` | `400` | Burst pro Org |
| `limits.org.in_flight` | `16` | Gleichzeitige Requests pro Org |
| `limits.principal.rate_per_second` | `20` | Requests/s pro Principal |
| `limits.principal.burst` | `40` | Burst pro Principal |
| `limits.principal.in_flight` | `4` | Gleichzeitige Requests pro Principal |

`limits.organizations.<name>` überschreibt `rate_per_second`, `burst` und
`in_flight` für eine Org. Der Name folgt den bestehenden Org-Namensregeln.
Validierung: alle Zahlen > 0, `queue_timeout_ms` darf `0` sein (dann kein
Warten), `max_entries` mindestens 100.

### Schlüssel und Speicher

- Quell-IP: direkter Peer ohne Trusted Proxy, sonst der `X-Forwarded-For`
  von rechts nach links, solange die Adresse in `server.trusted_proxies`
  liegt; die erste nicht vertrauenswürdige Adresse ist der Client. Ein vom
  Client vorangestellter XFF-Eintrag kann das Limit so nicht umgehen. IPv4
  wird exakt geschlüsselt, IPv6 auf /64 aggregiert.
- Org: `org.id`, unabhängig davon, ob der Pfad den Namen oder wie bei
  Bookshelf die ID trägt.
- Principal: `(org.id, principal.actor_id)`, nicht der Benutzername; ohne Org
  kein Principal-Bucket.
- Maps sind auf 16 Shards verteilt (`Mutex<HashMap<...>>`). Jeder Eintrag
  trägt `last_seen`; überschreitet eine Map `max_entries`, wirft sie die
  ältesten zehn Prozent unter demselben Lock weg. Einträge mit laufenden
  In-Flight-Permits werden übersprungen, damit ein neuer leerer Zähler das
  Limit nicht kurzzeitig verdoppelt; sind alle Einträge beschäftigt, darf die
  Map vorübergehend über `max_entries` liegen.
- Zeitbasis ist eine monotone `Instant`-Uhr; Buckets füllen kontinuierlich auf
  und deckeln bei `burst`.

### Antwort und Köpfe

- Status `503 Service Unavailable`.
- Körper wie die bestehende Überlastantwort:
  `{"error":["Server is overloaded. Retry later."]}`.
- `Retry-After: 1` (bei knappen Buckets die geschätzte Zeit bis zum nächsten
  Token, aufgerundet, mindestens 1 Sekunde).
- Ein Rejection-Log enthält Request-ID, Pfad, Methode, Scope, Org, Principal
  und `retry_after`.

### Metriken

`/_stats` und OTLP bekommen Zähler und ein Gauge:

- `limits_allowed_total{scope}`
- `limits_rejected_total{scope,org}`
- `limits_in_flight{scope}` als Gauge

Scopes: `global`, `source`, `org`, `principal`. `org` ist bei globalen und
Quell-Ablehnungen leer oder der Pfad-Org-Name, soweit vorhanden.

### Doku

`docs/deployment.md` bekommt einen Abschnitt „Request limits“ mit allen
Schlüsseln, der Isolation zwischen Orgs, `/ _stats`-Metriken, dem
`Retry-After`-Verhalten von Cinc, dem Ausschalten und dem Satz, dass Limits
pro Replica gelten. Die Env-Tabelle wird mit aufgenommen.
`packaging/microwave.toml.example` bekommt den kommentierten Block.

## Verifikation

- Unit-Tests: Bucket-Füllung, Burst-Deckel, Ablehnung, Eviction, geschätztes
  `Retry-After`, IP-Extraktion mit und ohne Trusted Proxy, Konfigvalidierung.
- Integrationstests:
  - eine Org über ihrer Rate bekommt `503` + `Retry-After`, eine zweite Org
    im selben Lauf nicht;
  - ein Principal über seinem Limit blockiert einen zweiten Principal
    derselben Org nicht;
  - globales Limit sättigt und liefert `503`, `/_status` bleibt erreichbar;
  - `limits.enabled = false` reicht unverändert durch;
  - ein Bookshelf-Upload zählt gegen die Org (kein Principal vorhanden);
  - MCP wird nach erfolgreicher Bearer-Auth gegen Org und Principal begrenzt;
  - `/_stats` zeigt Ablehnungen und In-Flight-Werte.
- `docker-load` bleibt mit angehobenen Limits grün. Maßstab für die
  Standardwerte: ein gesunder Lauf mit Defaults darf `limits`-`503` zeigen,
  aber keine Fehlschläge oder Timeouts; die Last-Compose hebt
  `limits.global.in_flight` passend zur Poolgröße an. Der Report bekommt eine
  eigene Zeile für Limit-Ablehnungen, damit sie nicht wie Solver-`503`
  aussehen.

## Offen für den Builder

- Ob das Rejection-Log pro Ablehnung oder gesampelt geschrieben wird.
- Ob der Org-Check bei der Antwort auf einen unbekannten Org-Pfad entfällt
  (die Auth liefert dort bereits `404`, bevor der Bucket greift); die
  Org-Quote für `/validate` ist entschieden und nicht offen.
- Ob In-Flight-Permits bei gestreamten Antworten bis zum Ende des Bodys
  gehalten werden müssen; die betroffenen Endpunkte puffern heute.
