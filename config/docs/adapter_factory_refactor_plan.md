# Smart Home Adapter Factory Refactor Plan

> **Goal:** Make new adapters require no source-code changes in `crates/core`, `crates/api`, or the main config schema.
> **Chosen approach:** compile-time adapter factory registry
> **Registration model:** distributed registration via `inventory`
> **Config model:** generic `[adapters.<name>]` map, parsed by each adapter crate

---

## Problem Statement

Today, adding a new adapter such as `zigbee2mqtt` requires editing multiple shared files:

- `crates/core/src/config.rs` for adapter-specific config structs and validation
- `crates/api/src/main.rs` for direct adapter imports and manual construction
- tests that assume a fixed adapter set

This creates unnecessary coupling between:

- the core runtime
- API composition
- adapter-specific configuration and validation

The runtime already supports `Vec<Box<dyn Adapter>>`, so the main remaining issue is composition and config ownership.

---

## Target Outcome

After this refactor:

- `core` knows only about the `Adapter` trait and adapter factory traits, not specific adapters.
- `api` builds adapters by iterating a registry of factories, not by importing adapter crates directly.
- adapter crates own their own config parsing and validation.
- the top-level config schema does not need new Rust structs for each new adapter.
- adding a new adapter should not require source edits in `core`, `api`, or the generic config model.

Practical limitation:

- a new adapter crate still needs to be included in the Cargo workspace and linked into the binary so its factory can register.
- this plan reduces shared source-file edits, but does not remove Cargo manifest changes.

Implemented link strategy:

- `crates/adapters` is the binary-facing aggregator crate that depends on adapter crates for side-effect registration.
- `crates/api` depends on `crates/adapters`, not on individual adapter crates.

---

## Non-Goals

- No runtime dynamic loading of shared libraries.
- No external process plugin protocol.
- No hot-plugging adapters from the filesystem at runtime.
- No change to the `Adapter` runtime contract itself unless needed for clean composition.
- No replacement of the current runtime, event bus, or persistence flow.

---

## High-Level Design

### Core idea

Each adapter crate will provide a factory object that:

- declares the adapter name
- accepts generic config data for that adapter
- validates and parses that config into adapter-specific types
- constructs `Box<dyn Adapter>`

These factories are registered at compile time using `inventory`.

At startup, the API will:

1. load generic adapter config entries
2. enumerate registered factories
3. match config sections to factories
4. build enabled adapters
5. produce adapter summaries without hard-coded imports

---

## Proposed Crate Responsibilities

### `crates/core`

Owns only generic runtime-facing abstractions:

- `Adapter`
- `AdapterFactory`
- adapter config value alias or helper types
- factory registration type consumed by `inventory`

Must not contain:

- `OpenMeteoConfig`
- `Zigbee2MqttConfig`
- adapter-specific validation logic
- adapter-specific construction logic

### `crates/api`

Owns composition only:

- load generic config
- discover factories
- build enabled adapters
- report unknown/misconfigured adapters clearly

Must not contain:

- `use adapter_open_meteo::OpenMeteoAdapter`
- `if config.adapters.open_meteo.enabled { ... }`
- adapter-specific config parsing

### `crates/adapters`

Owns binary linkage only:

- depends on adapter crates
- ensures their `inventory` registrations are linked into the final binary

Must not contain:

- runtime logic
- config parsing
- API composition logic

### adapter crates

Each adapter crate owns:

- its config struct
- its validation rules
- its `AdapterFactory` implementation
- its `inventory` registration

---

## Proposed Config Shape

Current direction:

```toml
[adapters.open_meteo]
enabled = true
latitude = 51.5
longitude = -0.1
poll_interval_secs = 90

[adapters.zigbee2mqtt]
enabled = true
base_url = "http://localhost:8080"
topic_prefix = "zigbee2mqtt"
poll_interval_secs = 30
```

Rust-side config model should become generic:

```rust
pub type AdapterConfigs = std::collections::HashMap<String, serde_json::Value>;

pub struct Config {
    pub runtime: RuntimeConfig,
    pub logging: LoggingConfig,
    pub persistence: PersistenceConfig,
    pub telemetry: TelemetryConfig,
    pub adapters: AdapterConfigs,
}
```

Notes:

- each adapter config remains nested under `[adapters.<name>]`
- adapter-specific fields are deserialized inside the adapter crate
- `enabled` remains adapter-owned so adapters can opt into custom defaults if needed

---

## Proposed Core Traits

Suggested location:

- `crates/core/src/adapter.rs` or a new `crates/core/src/adapter_factory.rs`

Suggested interface:

```rust
pub type AdapterConfigValue = serde_json::Value;

pub trait AdapterFactory: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn build(&self, config: AdapterConfigValue) -> anyhow::Result<Option<Box<dyn Adapter>>>;
}
```

Recommended contract:

- `name()` matches the TOML section key under `[adapters.<name>]`
- `build()` returns:
  - `Ok(Some(adapter))` when config is valid and enabled
  - `Ok(None)` when config is valid but disabled
  - `Err(...)` when config is invalid

Alternative split if clearer:

```rust
pub trait AdapterFactory: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn build(&self, config: AdapterConfigValue) -> anyhow::Result<Box<dyn Adapter>>;
    fn is_enabled(&self, config: &AdapterConfigValue) -> anyhow::Result<bool>;
}
```

Preferred first pass:

- keep a single `build()` entry point returning `Option<Box<dyn Adapter>>`
- this keeps enable/disable logic inside the adapter crate and minimizes API branching

---

## Proposed Registration Model

Use `inventory`.

Suggested pattern:

```rust
pub struct RegisteredAdapterFactory {
    pub factory: &'static dyn AdapterFactory,
}

inventory::collect!(RegisteredAdapterFactory);
```

Each adapter crate registers itself once:

```rust
inventory::submit! {
    RegisteredAdapterFactory {
        factory: &OPEN_METEO_FACTORY,
    }
}
```

Where `OPEN_METEO_FACTORY` is a `static` implementing `AdapterFactory`.

Binary linkage requirement:

- the final binary must link each adapter crate for `inventory` registration to be available.
- this is handled through `crates/adapters`.

---

## API Composition Flow After Refactor

Target startup flow:

1. load config
2. create persistence store
3. build adapters from registered factories and generic adapter config map
4. create runtime
5. hydrate registry from store
6. start persistence worker
7. start runtime
8. start HTTP server

Adapter build algorithm:

1. collect registered factories into a map by name
2. iterate configured adapter sections
3. look up matching factory
4. fail clearly if config contains an enabled adapter with no registered factory
5. call `build()`
6. add returned adapter if enabled
7. produce `AdapterSummary` from built adapters or config/factory name

Recommended failure behavior:

- invalid adapter config should fail startup clearly
- unknown configured adapter should fail startup clearly
- unconfigured registered adapters should be ignored

---

## Open-Meteo Migration Plan

The existing `adapter-open-meteo` crate should be converted first and serve as the template for future adapters.

### Move ownership into the crate

- move `OpenMeteoConfig` out of `core`
- define it inside `crates/adapter-open-meteo`
- derive `Deserialize`
- keep validation in the crate

### Add a factory

- implement `AdapterFactory` for an `OpenMeteoFactory`
- parse generic config into `OpenMeteoConfig`
- return `Ok(None)` when `enabled = false`
- build `OpenMeteoAdapter` when enabled

### Register the factory

- add `inventory::submit!` in the crate

---

## Phase Plan

## Phase 20 — Generic Adapter Config

**Goal:** remove adapter-specific config structs from `core`.

### Task 20.1 — Replace typed adapter config with a generic map

- remove `AdaptersConfig`
- remove `OpenMeteoConfig` from `core`
- change `Config.adapters` to a generic key/value map

### Task 20.2 — Keep generic config validation in `core`

`core` should validate only cross-cutting config such as:

- persistence
- logging if needed
- runtime settings

`core` should no longer validate adapter-specific fields.

**Acceptance Criteria:**

- `core` contains no adapter-specific config structs
- generic config still loads from `config/default.toml`
- Status: complete

---

## Phase 21 — Adapter Factory Abstraction

**Goal:** define generic construction interfaces in `core`.

### Task 21.1 — Add `AdapterFactory` support

- add trait and registration types to `core`
- add `inventory` support in the appropriate crate(s)

### Task 21.2 — Add helper for registered factory lookup

This can live in `core` or `api`.

Recommended direction:

- keep low-level registration types in `core`
- keep startup assembly logic in `api`

**Acceptance Criteria:**

- `core` compiles with no knowledge of concrete adapters
- factories can be enumerated at runtime
- Status: complete

---

## Phase 22 — Convert Open-Meteo To Factory Model

**Goal:** make the existing adapter use the new mechanism.

### Task 22.1 — Move config into `adapter-open-meteo`

- define `OpenMeteoConfig` in the adapter crate
- preserve current config keys and defaults where possible

### Task 22.2 — Implement `OpenMeteoFactory`

- parse generic config payload
- validate config
- return `Ok(None)` when disabled
- register with `inventory`

**Acceptance Criteria:**

- Open-Meteo builds without any config type in `core`
- Open-Meteo can still be enabled from `config/default.toml`
- Status: complete

---

## Phase 23 — Remove Adapter-Specific API Composition

**Goal:** make `api` fully adapter-agnostic.

### Task 23.1 — Remove direct adapter imports from `api`

- remove `use adapter_open_meteo::OpenMeteoAdapter`
- replace manual adapter creation with factory iteration

### Task 23.2 — Build adapter summaries generically

The API should derive summaries from the adapters it actually builds.

Suggested rule:

- built adapters appear as `running`
- configured but disabled adapters are omitted or shown as disabled based on current API preference

Preferred first pass:

- show only adapters that are actually built

**Acceptance Criteria:**

- `api` contains no adapter-specific startup logic
- app still starts with Open-Meteo enabled
- Status: complete

---

## Phase 24 — Unknown Adapter Handling

**Goal:** fail clearly when config and registration disagree.

### Task 24.1 — Unknown configured adapter

- if `[adapters.zigbee2mqtt]` exists but no `zigbee2mqtt` factory is linked, fail startup with a clear error

### Task 24.2 — Duplicate factory names

- detect duplicate registered names at startup
- fail clearly because adapter names are part of the device ID namespace

**Acceptance Criteria:**

- operator gets a clear startup error for unknown or duplicate adapter names
- Status: partially complete

Implemented:

- unknown configured adapter names fail clearly
- duplicate factory names fail clearly during startup assembly

Remaining:

- add an explicit duplicate-registration test if a clean simulation path is introduced

---

## Phase 25 — Tests

**Goal:** prove new adapters can be added without touching shared source files.

### Task 25.1 — Core/config tests

Add tests for:

- config loads generic adapter map
- adapter-specific validation no longer lives in `core`

### Task 25.2 — Open-Meteo factory tests

Add tests for:

- valid config builds adapter
- disabled config returns `None`
- invalid config fails clearly

### Task 25.3 — API composition tests

Add tests for:

- adapters are built from registered factories
- unknown adapter config fails clearly
- duplicate factory names fail clearly if simulated

**Acceptance Criteria:**

- `cargo check --workspace` passes
- `cargo clippy --workspace -- -D warnings` passes
- `cargo test --workspace` passes
- Status: complete, except duplicate-factory simulation coverage noted above

---

## Implementation Notes

- Keep the existing `Adapter` trait if possible.
- Do not move persistence logic into adapter crates.
- Keep device ID namespacing rules unchanged.
- Prefer the smallest working factory abstraction first.
- Avoid introducing a second layer of plugin traits unless clearly needed.

---

## Future Adapter Example

Desired future workflow for `zigbee2mqtt`:

1. create `crates/adapter-zigbee2mqtt`
2. implement `Zigbee2MqttConfig`
3. implement `Zigbee2MqttAdapter`
4. implement `Zigbee2MqttFactory`
5. register the factory with `inventory`
6. add the crate to the workspace/build
7. add the crate dependency to `crates/adapters/Cargo.toml`
8. add `[adapters.zigbee2mqtt]` config

No source edits should be required in:

- `crates/core/src/config.rs`
- `crates/api/src/main.rs`
- shared startup wiring

---

## Exit Criteria

This refactor is complete when:

- `core` has no adapter-specific config structs
- `api` has no adapter-specific imports or manual adapter construction
- `adapter-open-meteo` registers itself through the factory mechanism
- a second adapter crate can be added without changing `core` or `api` source files
- workspace checks, clippy, and tests pass

Current status:

- all of the above are complete except the repository does not yet include a second real adapter crate to prove the workflow end-to-end
