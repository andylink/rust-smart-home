# Smart Home

Smart Home is a Rust workspace for a small home-automation runtime with:

- an in-memory device registry as the hot read path
- an HTTP API for device inspection
- a WebSocket event stream for live updates
- adapter-driven device updates
- SQLite-backed current-state persistence for restart recovery

## Workspace Layout

```text
smart-home/
├── config/
│   ├── default.toml
│   └── docs/
├── crates/
│   ├── adapters/
│   ├── api/
│   ├── core/
│   ├── adapter-open-meteo/
│   └── store-sql/
└── README.md
```

## Current Persistence Model

- `DeviceRegistry` remains the in-memory runtime state used by the API and WebSocket paths.
- Persistence stores latest known device state, not event history.
- Startup hydrates the registry from SQLite before adapters begin polling.
- Live device changes are persisted asynchronously after the registry is updated.
- If the persistence subscriber lags and misses broadcast events, it reconciles from the registry back into the database.

This keeps reads fast while allowing the process to recover previously persisted state after restart.

## Adapter Architecture

Adapters are now built through a compile-time factory registry.

- `crates/core` defines the shared `Adapter` and `AdapterFactory` traits.
- each adapter crate owns its own config struct, validation, and factory registration.
- `crates/api` discovers registered factories and builds adapters from the generic `[adapters.<name>]` config map.
- `crates/adapters` is the binary link crate that pulls adapter crates into the final build so their registrations are available at runtime.

This removes adapter-specific startup logic from `api` and adapter-specific config types from `core`.

## Adapter Config Model

Adapter config is intentionally generic at the top level.

Example:

```toml
[adapters.open_meteo]
enabled = true
latitude = 51.5
longitude = -0.1
poll_interval_secs = 90
```

`core` treats each `[adapters.<name>]` section as untyped JSON-like config data.
Each adapter crate is responsible for deserializing and validating its own section.

## Adding A New Adapter

To add a new adapter such as `zigbee2mqtt`:

1. create `crates/adapter-zigbee2mqtt`
2. define that crate's config struct and validation rules
3. implement `Adapter`
4. implement `AdapterFactory`
5. register the factory with `inventory`
6. add the new crate to the workspace in `Cargo.toml`
7. add the new crate as a dependency in `crates/adapters/Cargo.toml`
8. add `[adapters.zigbee2mqtt]` to your TOML config

You should not need to edit:

- `crates/core/src/config.rs`
- `crates/api/src/main.rs`

## Unknown Adapter Handling

Startup fails clearly when:

- config references an adapter name that has no registered factory
- two linked adapter crates register the same adapter name

## Requirements

- Rust stable toolchain
- Cargo

SQLite is embedded through `sqlx` and does not require a separate database server.

## Running The API

From the workspace root:

```bash
cargo run -p api -- --config config/default.toml
```

The API binds to `127.0.0.1:3000`.

Useful endpoints:

- `GET /health`
- `GET /adapters`
- `GET /devices`
- `GET /devices/{id}`
- `GET /events`

## Default Persistence Config

`config/default.toml` enables SQLite persistence by default:

```toml
[persistence]
enabled = true
backend = "sqlite"
database_url = "sqlite://data/smart-home.db"
auto_create = true
```

Behavior:

- `enabled = true` turns on startup hydration and background persistence.
- `backend = "sqlite"` uses the implemented SQLite store.
- `database_url` points to the local SQLite file.
- `auto_create = true` creates the database file and `devices` table if missing.

The default database path is relative to the workspace root when you run the API there.

## SQLite Setup Notes

No manual schema bootstrap is required when `auto_create = true`.

On first startup, the app will create:

- the parent directory for the SQLite file if needed
- the SQLite database file if missing
- the `devices` table used for current device state

If you want to inspect the database manually:

```bash
sqlite3 data/smart-home.db
```

Example query:

```sql
SELECT device_id, kind, updated_at FROM devices ORDER BY device_id;
```

## Persistence Semantics

The persistence layer is intentionally current-state only.

- `DeviceAdded` writes the full device record.
- `DeviceStateChanged` reads the full canonical device from the registry, then writes it.
- `DeviceRemoved` deletes the device record.
- Telemetry/history storage is not implemented yet.

This means:

- `/devices` and `/events` remain backed by the in-memory runtime state.
- SQLite is the recovery copy used during startup.
- a sudden crash may still lose the newest in-memory update if it was not flushed yet.

## Restart Recovery

Restart flow:

1. load config
2. create the SQLite device store
3. create the runtime and registry
4. load persisted devices from SQLite
5. restore them into the in-memory registry without publishing live device events
6. start the persistence worker
7. start adapters and HTTP/WebSocket serving

This allows previously persisted devices to appear in `/devices` immediately after restart, before a new adapter poll cycle runs.

## Configuration Validation

Persistence config validation currently enforces:

- `persistence.database_url` must be present when persistence is enabled
- unsupported or unimplemented backends fail clearly

Adapter-specific validation is owned by each adapter crate.
For example, Open-Meteo still enforces `poll_interval_secs >= 60`, but that validation now lives in `adapter-open-meteo` rather than `core`.

`postgres` is reserved for future support and is not implemented yet.

## Development Commands

```bash
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

## Future Work

- PostgreSQL support behind the same `DeviceStore` trait
- telemetry/history storage
- configurable telemetry selection by adapter, device, or capability
- additional adapter crates such as Zigbee2MQTT using the factory pattern
