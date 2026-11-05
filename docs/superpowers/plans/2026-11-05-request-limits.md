# Request Limits Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Begrenze gleichzeitige und schnelle Requests pro Prozess, pro Organisation und pro Principal in-memory, damit rogue Clients weder ihre eigene Org noch andere Orgs aushungern.

**Architecture:** Ein globales Admission-Semaphore und ein vorauthentifizierter Quell-IP-Bucket schützen den Prozess; nach der Auth folgen ein orgweites und ein engeres Principal-Bucket. Alle Verstöße antworten `503` mit `Retry-After`, damit Cinc erneut versucht. Bookshelf und MCP nutzen dieselben Org-/Principal-Grenzen.

**Tech Stack:** Rust, Axum/Tower, Tokio (`Semaphore`, `timeout`), bestehende `microwave-config`- und `microwave-metrics`-Bausteine. Keine neuen Crates.

**Spec:** `docs/superpowers/specs/2026-11-05-request-limits-design.md`

## Global Constraints

- Keine neuen Crate-Abhängigkeiten.
- Limits gelten pro Prozess/Replica; es gibt keinen geteilten Zustand.
- Antwort auf jede Ablehnung: `503` mit `Retry-After`, Körper `{"error":["Server is overloaded. Retry later."]}`.
- Exempt bleiben `/_status`, `/_stats`, `/version`, `/server_api_version`, `/license`, die Landing-Page und ihre Assets.
- Der globale Default ist `32`; der Lasttest hebt ihn passend zur Poolgröße an.
- Org-Schlüssel ist `org.id`, Overrides matchen `org.name`; Principal-Schlüssel ist `(org.id, actor_id)`.
- IPv6 wird auf /64 aggregiert; `X-Forwarded-For` wird von rechts nach links über `server.trusted_proxies` gelaufen.
- `limits.enabled = false` kostet keinen zusätzlichen Arbeitsschritt.
- Metriken: `microwave_server_limits_total{scope,org,outcome}` und `microwave_server_limits_in_flight{scope}`; Scopes sind `global`, `source`, `org`, `principal`.
- Jede Task endet mit `cargo fmt --all` und einem fokussierten Commit.

## Review Focus

1. Ein Request ohne `ConnectInfo` (Tests, In-Process-Aufrufer) darf nicht alle Quellen in einen Bucket werfen; der Quell-Bucket wird dann übersprungen und nur das globale Limit greift. Test in Task 5.
2. Ein vom Client vorangestellter `X-Forwarded-For`-Eintrag darf das Limit nicht umgehen; nur die rechte, nicht vertrauenswürdige Adresse zählt. Test in Task 3.
3. `limits.enabled = false` liefert auch bei erschöpftem Bucket und belegtem Semaphore unverändert durch. Test in Task 5.
4. Eviction überspringt beschäftigte Einträge, damit ein geleerter Zähler das Limit nicht kurzzeitig verdoppelt; bei lauter beschäftigten Einträgen darf die Map über `max_entries` liegen. Test in Task 2.
5. `/_stats` bleibt bei gesättigtem globalem Limit erreichbar und die Labels wachsen nicht pro Principal. Tests in Task 4 und Task 5.

---

### Task 1: Konfiguration

**Files:**
- Modify: `crates/microwave-config/src/config.rs` (Config-Feld, Structs, Defaults, `validate`, `apply_env`, Env-Doku)
- Modify: `crates/microwave-config/src/lib.rs` (Re-Exports)
- Test: `crates/microwave-config/tests/config_test.rs`

**Interfaces:**
- Consumes: `valid_org_name`, `parse_value`, `parse_bool`, `ConfigError::InvalidValue`.
- Produces: `Config.limits: LimitsConfig`; `LimitsConfig`, `LimitsGlobalConfig`, `LimitsRateConfig`, `LimitsOrgConfig`, `LimitsPrincipalConfig`, `LimitsOrgOverride` mit `Default`; `LimitsConfig::validate() -> Result<(), (String, String)>`.

- [ ] **Step 1: Failing tests an `config_test.rs` anhängen**

```rust
#[test]
fn limits_defaults_and_validation() {
    let c = Config::default();
    assert!(c.limits.enabled);
    assert_eq!(c.limits.queue_timeout_ms, 1_500);
    assert_eq!(c.limits.max_entries, 10_000);
    assert_eq!(c.limits.global.in_flight, 32);
    assert_eq!(c.limits.org.in_flight, 16);
    assert_eq!(c.limits.principal.in_flight, 4);

    assert!(Config::from_toml_str("[limits]\nmax_entries = 99\n").is_err());
    assert!(Config::from_toml_str("[limits.global]\nin_flight = 0\n").is_err());
    assert!(Config::from_toml_str("[limits.source]\nburst = 0\n").is_err());
    assert!(Config::from_toml_str(
        "[limits.organizations.Bad_Org]\nburst = 5\n"
    )
    .is_err());

    let parsed = Config::from_toml_str(
        "[limits]\nenabled = false\n[limits.organizations.acme]\nrate_per_second = 5\nin_flight = 2\n",
    )
    .unwrap();
    assert!(!parsed.limits.enabled);
    assert_eq!(parsed.limits.organizations["acme"].rate_per_second, Some(5));
    assert_eq!(parsed.limits.organizations["acme"].in_flight, Some(2));
}

#[test]
fn limits_env_overrides() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("MW_LIMITS__GLOBAL__IN_FLIGHT", "7");
    std::env::set_var("MW_LIMITS__ORG__RATE_PER_SECOND", "11");
    let mut config = Config::default();
    config.apply_env().unwrap();
    std::env::remove_var("MW_LIMITS__GLOBAL__IN_FLIGHT");
    std::env::remove_var("MW_LIMITS__ORG__RATE_PER_SECOND");
    assert_eq!(config.limits.global.in_flight, 7);
    assert_eq!(config.limits.org.rate_per_second, 11);
}
```

- [ ] **Step 2: Tests laufen lassen und Fehler bestätigen**

Run: `cargo test -p microwave-config --test config_test limits_`
Expected: FAIL, `Config` hat kein Feld `limits`.

- [ ] **Step 3: Structs und Defaults implementieren**

In `config.rs` nahe `DepsolverConfig`, mit `#[serde(default)]` und den Werte aus der Spec:

```rust
pub struct LimitsConfig {
    pub enabled: bool,
    pub queue_timeout_ms: u64,
    pub max_entries: usize,
    pub global: LimitsGlobalConfig,
    pub source: LimitsRateConfig,
    pub org: LimitsOrgConfig,
    pub principal: LimitsPrincipalConfig,
    pub organizations: std::collections::BTreeMap<String, LimitsOrgOverride>,
}
```

`LimitsGlobalConfig { in_flight }`, `LimitsRateConfig { rate_per_second: u32, burst: u32 }`, `LimitsOrgConfig { rate_per_second: u32, burst: u32, in_flight: usize }`, `LimitsPrincipalConfig` wie Org, `LimitsOrgOverride` mit `Option<u32>`/`Option<usize>`. `Default` setzt Spec-Werte (`enabled: true`, `queue_timeout_ms: 1_500`, `max_entries: 10_000`, global 32, source 500/1000, org 200/400/16, principal 20/40/4).

Feld `pub limits: LimitsConfig` in `Config` ergänzen.

- [ ] **Step 4: Validierung implementieren**

`LimitsConfig::validate` prüft: `max_entries >= 100`; chaque Scope-Rate/Burst/`in_flight` `> 0`; Override-Werte nur wenn gesetzt `> 0`; Org-Namen über `valid_org_name`. Rückgabe-Keys exakt `limits.global.in_flight`, `limits.organizations.<name>.<feld>`. In `Config::validate` vor `Ok(())` aufrufen und wie `mail` auf `ConfigError::InvalidValue` mappen.

- [ ] **Step 5: Env-Overrides implementieren**

Die elf Variablen aus dem Spec-Design (`MW_LIMITS__ENABLED`, `MW_LIMITS__QUEUE_TIMEOUT_MS`, `MW_LIMITS__MAX_ENTRIES`, `MW_LIMITS__GLOBAL__IN_FLIGHT`, `MW_LIMITS__SOURCE__{RATE_PER_SECOND,BURST}`, `MW_LIMITS__ORG__{RATE_PER_SECOND,BURST,IN_FLIGHT}`, `MW_LIMITS__PRINCIPAL__{RATE_PER_SECOND,BURST,IN_FLIGHT}`) in `apply_env` ergänzen; bool via `parse_bool`, Zahlen via `parse_value`. Die `apply_env`-Doku-Liste ergänzen.

- [ ] **Step 6: Re-Exports und Tests**

`lib.rs` exportiert die neuen Typen. Run: `cargo test -p microwave-config --test config_test limits_`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/microwave-config
git commit -m "feat(config): add request limit settings"
```

---

### Task 2: Limiter-Kern (Buckets, Permits, Eviction)

**Files:**
- Create: `crates/microwave-api/src/limits/mod.rs`
- Create: `crates/microwave-api/src/limits/bucket.rs`
- Modify: `crates/microwave-api/src/lib.rs` (`pub mod limits;`)
- Test: Inline-`#[cfg(test)]` in `bucket.rs`

**Interfaces:**
- Consumes: `microwave_config::{LimitsConfig, LimitsOrgOverride}`.
- Produces:
  - `pub enum Scope { Global, Source, Org, Principal }` mit `as_str()`
  - `pub struct Rejection { pub scope: Scope, pub retry_after: u64 }`
  - `pub struct LimitPermit` (leer, wenn Limits aus sind)
  - `pub struct Limiter` mit `enabled()`, `has_org_overrides()`, `global() -> Result<LimitPermit, Rejection>`, `source(IpAddr)`, `org(&Org)`, `org_by_key(&str, Option<&str>)`, `principal(&Org, &Principal)`
  - `bucket::ShardedMap<K>` und `bucket::Slot`

- [ ] **Step 1: Failing tests für den Bucket schreiben**

Tests decken ab: Burst wird verbraucht, danach `Rejection` mit `retry_after >= 1`; `burst` deckelt die Füllung; `take_token` mit vorgegebenen `Instant`-Werten füllt kontinuierlich nach; Eviction entfernt den ältesten **idle** Eintrag und lässt den beschäftigten stehen; sind alle Einträge beschäftigt, wächst die Map über `max_entries`.

```rust
#[test]
fn burst_is_capped_and_rejection_names_a_retry_after() {
    let map = ShardedMap::<u32>::new(100);
    let spec = RateSpec { rate_per_second: 1.0, burst: 2.0 };
    let now = Instant::now();
    assert!(map.acquire(1, spec, 1, now).is_ok());
    assert!(map.acquire(1, spec, 1, now).is_ok());
    let rejection = map.acquire(1, spec, 1, now).err().unwrap();
    assert!(rejection.retry_after >= 1);
    assert!(map.acquire(1, spec, 1, now + Duration::from_secs(3)).is_ok());
}
```

- [ ] **Step 2: Tests laufen lassen**

Run: `cargo test -p microwave-api limits::bucket`
Expected: FAIL, Modul fehlt.

- [ ] **Step 3: `bucket.rs` implementieren**

`RateSpec` kopierbar; `Bucket { tokens: f64, burst: f64 }` mit `try_take(rate: f64, now: Instant) -> Result<(), u64>` (kontinuierliche Füllung, Deckel `burst`, `retry_after = ((1.0 - tokens) / rate).ceil().max(1.0) as u64`). `Entry { bucket, in_flight: Arc<Semaphore>, busy: AtomicUsize, last_seen: Instant }`. `Slot(Arc<Entry>)`; `Drop for Slot` dekrementiert `busy`. `ShardedMap` mit 16 `Mutex<HashMap<K, Arc<Entry>>>`; Shard-Kapazität `max_entries.div_ceil(16).max(1)`; `acquire` legt an, aktualisiert `last_seen`, evictet vor dem Token-Take (älteste 10 %, `busy == 0` überspringen) und gibt `Slot` plus `Arc<Semaphore>` zurück.

- [ ] **Step 4: `Limiter` in `mod.rs` implementieren**

`Limiter::new(&LimitsConfig, Arc<Metrics>)` hält `enabled`, `queue_timeout`, Quoten und Overrides. `global()` nimmt `try_acquire_owned`; bei Belegung `tokio::time::timeout(queue_timeout, acquire_owned)`; Timeout → `Rejection { retry_after: 1 }`. `source`/`org`/`principal` holen den Slot aus ihrer Map, nehmen einen Token (Rate-Verstoß → `Rejection` mit berechnetem `retry_after`), warten auf das Semaphore und verpacken beides in `LimitPermit`. Bei `!enabled` geben alle Methoden sofort ein leeres `LimitPermit` zurück. Über `org_by_key` wird der Override über den optionalen Namen aufgelöst, sonst gelten die Defaults.

`Lib.rs` bekommt `pub mod limits;`.

- [ ] **Step 5: Tests laufen lassen**

Run: `cargo test -p microwave-api limits::bucket`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/microwave-api/src/limits crates/microwave-api/src/lib.rs
git commit -m "feat(limits): add token buckets and bounded in-flight limiter"
```

---

### Task 3: Quell-IP-Ermittlung

**Files:**
- Create: `crates/microwave-api/src/limits/source.rs`
- Modify: `crates/microwave-api/src/middleware.rs` (`proxy_matches` → `pub(crate)`)
- Test: Inline-`#[cfg(test)]` in `source.rs`

**Interfaces:**
- Consumes: `middleware::proxy_matches`, `ConnectInfo<SocketAddr>`.
- Produces: `pub(crate) enum IpKey { V4(u32), V6(u64) }` und `pub(crate) fn client_ip(request: &Request, trusted: &[String]) -> Option<IpAddr>`.

- [ ] **Step 1: Failing tests schreiben**

```rust
#[test]
fn untrusted_peer_ignores_forwarded_for() {
    let peer: IpAddr = "203.0.113.9".parse().unwrap();
    assert_eq!(client(peer, Some("198.51.100.1"), &[]), Some(peer));
}

#[test]
fn trusted_peer_uses_the_rightmost_untrusted_address() {
    let peer: IpAddr = "127.0.0.1".parse().unwrap();
    let trusted = vec!["127.0.0.1".to_string(), "10.0.0.0/8".to_string()];
    assert_eq!(
        client(peer, Some("1.2.3.4, 10.0.0.5, 127.0.0.1"), &trusted),
        Some("1.2.3.4".parse().unwrap())
    );
    assert_eq!(
        client(peer, Some("1.2.3.4, 127.0.0.1"), &trusted),
        Some("1.2.3.4".parse().unwrap())
    );
}

#[test]
fn ipv6_keys_on_the_64_bit_prefix() {
    let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
    let b: IpAddr = "2001:db8:1:2::ffff".parse().unwrap();
    let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
    assert_eq!(IpKey::from(a), IpKey::from(b));
    assert_ne!(IpKey::from(a), IpKey::from(c));
}
```

- [ ] **Step 2: Tests laufen lassen**

Run: `cargo test -p microwave-api limits::source`
Expected: FAIL, Modul fehlt.

- [ ] **Step 3: Implementieren**

`client_ip` liest den Peer aus `ConnectInfo<SocketAddr>` oder `SocketAddr` in den Extensions. Ohne Peer → `None`. Ist der Peer nicht vertrauenswürdig → Peer. Sonst `X-Forwarded-For` an Kommas von rechts nach links: erste Adresse, die nicht `proxy_matches` gegen `trusted` erfüllt, ist der Client; alle vertrauenswürdig oder unparsebar → Peer. `IpKey::from(IpAddr)` mappt IPv4 auf `V4(u32)` und IPv6 auf die oberen 64 Bit als `V6(u64)`; `IpKey` leitet `Clone, Copy, Debug, PartialEq, Eq, Hash` ab.

Testhelfer `client(peer, forwarded, trusted)` baut einen `Request` mit `ConnectInfo(SocketAddr)`, setzt optional `X-Forwarded-For` und ruft `client_ip` auf.

`middleware::proxy_matches` auf `pub(crate)` heben.

- [ ] **Step 4: Tests laufen lassen**

Run: `cargo test -p microwave-api limits::source`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/microwave-api/src/limits/source.rs crates/microwave-api/src/middleware.rs
git commit -m "feat(limits): derive client address behind trusted proxies"
```

---

### Task 4: Metriken

**Files:**
- Modify: `crates/microwave-metrics/src/registry.rs`
- Modify: `crates/microwave-metrics/src/recorder.rs`
- Modify: `crates/microwave-metrics/src/lib.rs`
- Modify: `crates/microwave-api/src/lib.rs` (`/_stats` Text- und JSON-Ausgabe)
- Modify: `crates/microwave-otel/src/lib.rs` (Katalog)
- Modify: `crates/microwave-otel/src/recorder.rs`
- Modify: `crates/microwave-otel/src/gauges.rs`
- Test: Unit-Tests in `registry.rs`, `lib.rs` und `recorder.rs`

**Interfaces:**
- Consumes: `Counters`, `Metrics`, `Recorder`.
- Produces: `LimitSeries { scope, org, outcome }`; `Metrics::record_limit(scope, org, outcome)`, `limit_series()`, `adjust_limits_in_flight(scope, delta)`, `limits_in_flight() -> Vec<(&'static str, u64)>`; `Recorder::limit(scope, org, outcome)`.

- [ ] **Step 1: Failing Tests schreiben**

Registry: `record_limit("org", "acme", "rejected")` erscheint als Serie; `adjust_limits_in_flight("org", 1)` und `-1` liefern den Gauge-Wert. `lib.rs`: `stats_json` enthält `limits.totals` und `limits.in_flight.org`; die Textausgabe enthält `microwave_server_limits_total{scope="org",org="acme",outcome="rejected"}`. OTel: der Recorder exportiert `microwave_server_limits_total` mit `scope`/`org`/`outcome` und der Katalog-Gauge `microwave_server_limits_in_flight` wird registriert.

- [ ] **Step 2: Tests laufen lassen**

Run: `cargo test -p microwave-metrics limits` und `cargo test -p microwave-api limits`
Expected: FAIL.

- [ ] **Step 3: Registry implementieren**

`histogram_series! { LimitSeries { scope: String, org: String, outcome: String } collapsed: scope with OVERFLOW_LABEL }`, `Counters<LimitSeries>` im `Metrics`-Struct, `BTreeMap<&'static str, u64>` hinter `Mutex`. `record_limit` zählt und ruft `recorder.limit(...)`; die Gauge-Methoden sättigen bei 0. `limit_series()` sortiert nach `(scope, org, outcome)`. Die Test-Implementierung `Counting` in `registry.rs` bekommt `limit` ebenfalls.

- [ ] **Step 4: `/_stats` erweitern**

Text: `# HELP/# TYPE microwave_server_limits_total counter` je Serie, danach Gauge `microwave_server_limits_in_flight{scope="..."} <n>`. JSON: `"limits": { "in_flight": { "<scope>": n }, "totals": [ {scope, org, outcome, count} ] }`. `stats_json` liest die Werte selbst aus `metrics`.

- [ ] **Step 5: OTLP ergänzen**

`CATALOGUE` und `recorder::INSTRUMENTS` um `("microwave.limits.requests", "microwave_server_limits_total")` erweitern; `OtelRecorder` bekommt `limits: Counter<u64>` und implementiert `limit`. `gauges::INSTRUMENTS`/`CATALOGUE` um `microwave.limits.in_flight` ergänzen und einen Observable-Gauge registrieren, der `metrics.limits_in_flight()` als `scope`-Labels beobachtet.

- [ ] **Step 6: Tests laufen lassen**

Run: `cargo test -p microwave-metrics limits && cargo test -p microwave-api limits && cargo test -p microwave-otel`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/microwave-metrics crates/microwave-api/src/lib.rs crates/microwave-otel
git commit -m "feat(metrics): expose request limit decisions"
```

---

### Task 5: Aufnahme-Layer (global und Quell-IP)

**Files:**
- Create: `crates/microwave-api/src/limits/layer.rs`
- Modify: `crates/microwave-api/src/limits/mod.rs` (`pub mod layer;`, Re-Exports)
- Modify: `crates/microwave-api/src/state.rs` (Feld und Konstruktion)
- Modify: `crates/microwave-api/src/lib.rs` (Layer und Exempt-Liste)
- Modify: `crates/microwave-api/src/middleware.rs` (`serves_landing_page`, `is_static_asset_path`, `is_maintenance_exempt` → `pub(crate)`)
- Test: `crates/microwave-api/tests/limits_test.rs`

**Interfaces:**
- Consumes: `Limiter`, `source::client_ip`, `AppState`.
- Produces: `pub async fn admission(State<AppState>, Request, Next) -> Response`; `pub fn overloaded(retry_after: u64) -> Response`; `RequestContext`-Marker `AdmissionApplied`.

- [ ] **Step 1: Failing Integrationstests schreiben**

In `limits_test.rs`: Helfer `config()` setzt `queue_timeout_ms = 50` und einen großen Quell-Burst; `peer(request, addr)` fügt `ConnectInfo(SocketAddr)` ein. `create_org(&app, name)` legt die Org an, bevor ein Test den globalen Slot belegt.

```rust
#[tokio::test]
async fn saturated_global_limit_answers_503_and_keeps_health_reachable() {
    let mut config = config();
    config.limits.global.in_flight = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&app, "acme").await;

    let _held = state.limits.global().await.unwrap();
    let response = node_get(&app, "acme", "127.0.0.1").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    let body = support::response_json(response).await;
    assert_eq!(body, json!({ "error": ["Server is overloaded. Retry later."] }));

    let status = app
        .clone()
        .oneshot(Request::get("/_status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
}

#[tokio::test]
async fn requests_without_a_peer_skip_only_the_source_bucket() {
    let mut config = config();
    config.limits.source.burst = 1;
    config.limits.source.rate_per_second = 1;
    let state = support::state_with_config(config);
    let app = support::app(state);
    create_org(&app, "acme").await;

    assert_eq!(node_get_without_peer(&app, "acme").await.status(), StatusCode::OK);
    assert_eq!(node_get_without_peer(&app, "acme").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn disabled_limits_pass_a_saturated_global_slot() {
    let mut config = config();
    config.limits.enabled = false;
    config.limits.global.in_flight = 1;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&app, "acme").await;
    let _held = state.limits.global().await.unwrap();
    assert_eq!(node_get(&app, "acme", "127.0.0.1").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn source_bucket_rejects_one_address_without_touching_another() {
    let mut config = config();
    config.limits.source.burst = 1;
    config.limits.source.rate_per_second = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state);
    create_org(&app, "acme").await;

    assert_eq!(node_get(&app, "acme", "203.0.113.1").await.status(), StatusCode::OK);
    assert_eq!(
        node_get(&app, "acme", "203.0.113.1").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(node_get(&app, "acme", "203.0.113.2").await.status(), StatusCode::OK);
}
```

`node_get` signiert `GET /organizations/{org}/nodes` mit `support::signed_request` und hängt `ConnectInfo` an; `node_get_without_peer` lässt es weg.

- [ ] **Step 2: Tests laufen lassen**

Run: `cargo test -p microwave-api --test limits_test`
Expected: FAIL.

- [ ] **Step 3: `layer.rs` implementieren**

```rust
#[derive(Clone, Copy)] pub struct AdmissionApplied;

pub fn is_exempt(request: &Request) -> bool { ... }
pub fn overloaded(retry_after: u64) -> Response { ... }
pub fn rejected(rejection: Rejection, metrics: &Metrics, org: &str) -> Response { ... }

pub async fn admission(State(state), mut request, next) -> Response { ... }
```

`admission` prüft zuerst den Marker (Router-Reentry durch Default-Org/Trailing-Slash), dann `enabled` und `is_exempt`. Exempt: `/_status`, `/_stats`, `/version`, `/server_api_version`, `/license`, `is_static_asset_path`, sowie `/` wenn `serves_landing_page`. Danach Marker setzen, globalen Permit holen (Ablehnung → `rejected`), `client_ip` bestimmen; ohne IP direkt weiter, sonst Quell-Permit holen. Beide Permits über `next.run(...).await` halten.

`rejected` zählt `rejected_total` mit `scope` und `org` und schreibt eine `tracing::warn!`-Zeile mit `request_id`, `method`, `path`, `scope`, `org`, `principal` (soweit vorhanden) und `retry_after`. Erfolgreiche Aufnahmen zählen `allowed_total`; der In-Flight-Gauge wird beim Nehmen und Fallenlassen des Permits angepasst.

- [ ] **Step 4: State und Layer verdrahten**

`AppState` bekommt `pub limits: Arc<crate::limits::Limiter>`; `with_metrics` baut es aus `config.limits` und `metrics.clone()` **vor** dem Struct-Literal. In `lib.rs` die Layer-Kette zwischen `compress_response` und `observe_request` um `.layer(axum::middleware::from_fn_with_state(request_state.clone(), limits::admission))` ergänzen: `observe_request` bleibt die äußerste Lage, damit Ablehnungen im Zugriffslog und in der Antwortstatistik landen, `admission` bleibt aber vor der Authentifizierung.

- [ ] **Step 5: Tests laufen lassen**

Run: `cargo test -p microwave-api --test limits_test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/microwave-api/src crates/microwave-api/tests/limits_test.rs
git commit -m "feat(limits): admit requests globally and by source address"
```

---

### Task 6: Org- und Principal-Grenze nach der Auth

**Files:**
- Modify: `crates/microwave-api/src/limits/layer.rs`
- Modify: `crates/microwave-api/src/middleware.rs` (`authenticate_signed`)
- Test: `crates/microwave-api/tests/limits_test.rs`

**Interfaces:**
- Consumes: `AuthContext`-Bestandteile `Org`, `Principal`; `Limiter::org`, `Limiter::principal`.
- Produces: `pub async fn check_org_principal(state: &AppState, org: Option<&Org>, principal: &Principal) -> Result<(LimitPermit, LimitPermit), Response>`.

- [ ] **Step 1: Failing Tests schreiben**

Zwei Orgs `acme`/`other` mit `org.burst = 1` und hohem `org.in_flight`; erster Request auf `acme` `200`, zweiter `503`, `other` `200`. Principal-Test: Org-Bucket groß, `principal.burst = 1`, zwei User `alice`/`bob` in derselben Org; zweiter Request von `alice` `503`, erster von `bob` `200`. Zusätzlich: Request auf unbekannte Org bleibt `404` und zählt nicht in einen Org-Bucket. Ein `POST /organizations/acme/validate` verbraucht ebenfalls einen Org-Token (kein Sonderpfad).

- [ ] **Step 2: Tests laufen lassen**

Run: `cargo test -p microwave-api --test limits_test org_`
Expected: FAIL.

- [ ] **Step 3: `check_org_principal` implementieren**

Ohne Org → beide Permits leer. Sonst `org(org)` und danach `principal(org, principal)`; schlägt der Principal-Check fehl, fällt der Org-Permit mit dem `Result`-Fehler weg. Jede erfolgreiche Aufnahme zählt `allowed` mit dem passenden Scope und `org.name`; Ablehnungen laufen über `rejected`.

- [ ] **Step 4: In der Auth verdrahten**

In `authenticate_signed` nach der Principal-Ermittlung und vor `action_meta`:

```rust
let (_org_permit, _principal_permit) =
    match crate::limits::layer::check_org_principal(&state, org.as_ref(), &principal).await {
        Ok(permits) => permits,
        Err(response) => return Ok(response),
    };
```

Permits bleiben bis zum Ende von `next.run` in Scope.

- [ ] **Step 5: Tests laufen lassen**

Run: `cargo test -p microwave-api --test limits_test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/microwave-api/src/limits/layer.rs crates/microwave-api/src/middleware.rs crates/microwave-api/tests/limits_test.rs
git commit -m "feat(limits): bound organizations and principals after auth"
```

---

### Task 7: Bookshelf-Uploads und -Downloads

**Files:**
- Modify: `crates/microwave-api/src/limits/layer.rs`
- Modify: `crates/microwave-api/src/bookshelf.rs` (`TokenQuery`/`authorized` auf `pub(crate)`, Gate in `routes_with_limit`)
- Test: `crates/microwave-api/tests/limits_test.rs`

**Interfaces:**
- Consumes: `bookshelf::authorized`, `bookshelf::TokenQuery`, `Limiter::org_by_key`.
- Produces: `pub async fn bookshelf_org_gate(State<AppState>, Request, Next) -> Response`.

- [ ] **Step 1: Failing Test schreiben**

Baue den Pfad wie `bookshelf_contract_test` mit `bookshelf_token(SECRET, method, ORG, checksum, expiry)`; Bookshelf ist token-, nicht signaturauthentifiziert. `org.in_flight = 1`, `queue_timeout_ms = 0`. Belege den Org-Permit über `state.limits.org_by_key(ORG, None).await.unwrap()` und sende `PUT /bookshelf/organizations/<id>/checksums/<md5>?token=…&exp=…` → `503` mit `Retry-After`. Ohne Belegung → `202` wie der Contract-Test. Ein zweiter Test hält den Permit und prüft, dass ein `GET` auf denselben Pfad ebenfalls `503` liefert (Downloads zählen).

- [ ] **Step 2: Test laufen lassen**

Run: `cargo test -p microwave-api --test limits_test bookshelf`
Expected: FAIL.

- [ ] **Step 3: Gate implementieren und einhängen**

`bookshelf_org_gate` extrahiert Org-ID und Checksum aus den Pfadsegmenten, liest `Query<TokenQuery>`, ruft `bookshelf::authorized` und gibt bei Fehler die unveränderte Antwort zurück. HEAD prüft wie der Handler mit `GET`. Nur wenn `state.limits.has_org_overrides()` gilt, wird `find_org_by_id` für den Override-Namen aufgerufen. Dann `org_by_key` und Permit über `next.run` halten.

`routes_with_limit` nimmt zusätzlich den `AppState`; in `routes_with_limit` `.route_layer(axum::middleware::from_fn_with_state(state, bookshelf_org_gate))` ergänzen; das Gate liegt damit vor dem `Bytes`-Extraktor und vor `DefaultBodyLimit`. Der Contract-Test reicht den State an `routes_with_limit` und behält `with_state` für den Rest.

- [ ] **Step 4: Tests laufen lassen**

Run: `cargo test -p microwave-api --test limits_test bookshelf && cargo test -p microwave-api --test bookshelf_contract_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/microwave-api/src/limits/layer.rs crates/microwave-api/src/bookshelf.rs crates/microwave-api/tests/limits_test.rs
git commit -m "feat(limits): count bookshelf transfers against the org quota"
```

---

### Task 8: MCP nach Bearer-Auth

**Files:**
- Modify: `crates/microwave-api/src/mcp/mod.rs`
- Test: `crates/microwave-api/tests/mcp_test.rs`

**Interfaces:**
- Consumes: `McpContext { org, principal }`, `Limiter::org` und `Limiter::principal`.
- Produces: keine neue Signatur; der Handler hält beide Permits bis zur Antwort.

- [ ] **Step 1: Failing Test schreiben**

Mit der bestehenden `mcp_config`-Fixture: `org.burst = 1`; erster `POST /organizations/acme/mcp` mit Bearer-Token liefert die normale JSON-RPC-Antwort, der zweite `503` mit `Retry-After`. Ein Request mit falschem Token bleibt `401` und verbraucht keinen Org-Token.

- [ ] **Step 2: Test laufen lassen**

Run: `cargo test -p microwave-api --test mcp_test limit`
Expected: FAIL.

- [ ] **Step 3: Implementieren**

In `handle` direkt nach erfolgreichem `auth::authenticate`:

```rust
let _org_permit = match state.limits.org(&ctx.org).await {
    Ok(permit) => permit,
    Err(rejection) => return crate::limits::layer::rejected(rejection, &state.metrics, &ctx.org.name),
};
let _principal_permit = match state.limits.principal(&ctx.org, &ctx.principal).await {
    Ok(permit) => permit,
    Err(rejection) => return crate::limits::layer::rejected(rejection, &state.metrics, &ctx.org.name),
};
```

- [ ] **Step 4: Tests laufen lassen**

Run: `cargo test -p microwave-api --test mcp_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/microwave-api/src/mcp/mod.rs crates/microwave-api/tests/mcp_test.rs
git commit -m "feat(limits): bound mcp requests after bearer auth"
```

---

### Task 9: Dokumentation und Beispielkonfiguration

**Files:**
- Modify: `docs/deployment.md` (neuer Abschnitt nach „Mail (SMTP)“, Env-Tabelle)
- Modify: `packaging/microwave.toml.example` (kommentierter Block)
- Test: `microwave-ctl --config packaging/microwave.toml.example check-config`

**Interfaces:**
- Consumes: alle Task-1-Schlüssel.
- Produces: keine.

- [ ] **Step 1: Beispielblock schreiben**

Kommentierter Block nach `[depsolver]`, inaktive Werte:

```toml
# In-memory request limits, per process. Off disables every gate.
# [limits]
# enabled = true
# queue_timeout_ms = 1500
# max_entries = 10000
# [limits.global]
# in_flight = 32
# [limits.org]
# rate_per_second = 200
# burst = 400
# in_flight = 16
# [limits.principal]
# rate_per_second = 20
# burst = 40
# in_flight = 4
# [limits.organizations.acme]
# rate_per_second = 50
# in_flight = 8
```

- [ ] **Step 2: Doku-Abschnitt schreiben**

Aufbau: Zweck und Isolationsmodell (global vs. Org vs. Principal), `503` + `Retry-After` und warum Cinc nur `50x` wiederholt, exempt-Pfade, Bücher-Uploads/Downloads und MCP, Konfigtabelle mit allen elf Schlüsseln plus Overrides, XFF-Verhalten über `server.trusted_proxies`, IPv6-/64, `limits.enabled = false`, und ausdrücklich: **Limits gelten pro Prozess/Replica; mehrere Replicas teilen sich keinen Zähler.** Auf den `/_stats`-Abschnitt mit den drei Metriken verlinken.

- [ ] **Step 3: Tests laufen lassen**

Run: `microwave-ctl --config packaging/microwave.toml.example check-config`
Expected: PASS; der kommentierte Block ändert nichts, die aktiven Defaults bleiben gültig.

- [ ] **Step 4: Commit**

```bash
git add docs/deployment.md packaging/microwave.toml.example
git commit -m "docs(limits): document request limits and their knobs"
```

---

### Task 10: Lasttest-Report und Last-Stack

**Files:**
- Modify: `crates/microwave-load/src/retry.rs`
- Modify: `crates/microwave-load/src/metrics.rs`
- Modify: `crates/microwave-load/src/report.rs`
- Modify: `crates/microwave-load/src/sequence.rs` (Weitergabe des Zählers)
- Modify: `crates/microwave-load/src/lib.rs` (`LoadError::counted`)
- Modify: `docker-load/docker-compose.yml` (`MW_LIMITS__GLOBAL__IN_FLIGHT`)
- Modify: `docker-load/microwave/microwave.toml` (`[limits]`-Block)
- Modify: `docs/load-test.md` (Reportspalte und Limit-Hinweis)
- Test: Unit-Tests in `retry.rs`, `report.rs`

**Interfaces:**
- Consumes: Body-Marker `Server is overloaded`.
- Produces: `LimitOverloaded(pub u32)` parallel zu `SolverOverloaded`, `Sample.limit_503`, `Endpoint.limit_503`, `Summary.limit_503`, `EndpointResult.limit_503`; Reportzeile `limits_503`.

- [ ] **Step 1: Failing Tests schreiben**

`retry.rs`: `is_limit_overload(b"{\"error\":[\"Server is overloaded. Retry later.\"]}")` ist wahr, `b"busy"` falsch; `execute` zählt eine `503`-Limit-Antwort in `LimitOverloaded`, nicht in `SolverOverloaded`. `report.rs`: die Markdown-Tabelle enthält `limits_503` und die JSON-Zeile den Wert.

- [ ] **Step 2: Tests laufen lassen**

Run: `cargo test -p microwave-load limit`
Expected: FAIL.

- [ ] **Step 3: Zähler durchreichen**

Analog zu `SolverOverloaded` in `execute`, `LoadError::counted`, `Sample`/`Endpoint`/`Summary`/`EndpointResult` und der Markdown-/JSON-Ausgabe. Der Body-Marker ist `LIMIT_OVERLOADED: &[u8] = b"Server is overloaded"`.

- [ ] **Step 4: Stack anheben**

In `docker-load/microwave/microwave.toml` einen `[limits]`-Block mit `queue_timeout_ms = 1500` und `[limits.global] in_flight = 64` ergänzen; in `docker-compose.yml` `MW_LIMITS__GLOBAL__IN_FLIGHT: ${MW_LIMITS_GLOBAL_IN_FLIGHT:-64}` setzen. In `docs/load-test.md` die Spalte und erklären, dass Limit-`503` von Solver-`503` getrennt gezählt werden.

- [ ] **Step 5: Tests laufen lassen**

Run: `cargo test -p microwave-load && cargo fmt --all && cargo clippy --workspace --all-targets`
Expected: PASS, keine Clippy-Warnungen.

- [ ] **Step 6: Commit**

```bash
git add crates/microwave-load docker-load docs/load-test.md
git commit -m "test(load): count request-limit rejections separately"
```

---

### Abschluss

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets`
- [ ] `scripts/run-pedant.sh <betroffene Suites>`
- [ ] Whole-Branch-Review
