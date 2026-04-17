# Dashboard Template Guide

This guide describes the recommended pattern for building small external dashboards against the Smart Home API.

The reference implementation lives in:

- `examples/dashboard-template/`

http://127.0.0.1:8080/examples/dashboard-template/?api=http://127.0.0.1:3001

## Why The Dashboard Lives Outside `crates/api`

`crates/api` should remain focused on runtime startup plus the HTTP and WebSocket system contract.

The dashboard template is intentionally external so that:

- the API stays lean and reusable
- UI experiments do not expand the backend surface area
- AI or MCP tooling can treat the API as the stable contract and generate dashboards separately

## Minimal Dashboard Contract

The reference template uses only two runtime interfaces:

1. filtered `GET /devices`
2. WebSocket `GET /events`

Example filtered request:

```text
GET /devices?ids=open_meteo:temperature_outdoor&ids=open_meteo:wind_speed&ids=open_meteo:wind_direction
```

This allows a dashboard to fetch only the devices it needs without adding dashboard-specific API endpoints.

For browser-based dashboards served from another origin, enable explicit API CORS origins in `config/default.toml`:

```toml
[api.cors]
enabled = true
allowed_origins = ["http://127.0.0.1:8080"]
```

The API keeps CORS disabled by default.

For the reference template in `examples/dashboard-template/`, the usual local pairing is:

- dashboard: `http://127.0.0.1:8080`
- API: `http://127.0.0.1:3001`

## Recommended Frontend Pattern

Use this flow:

1. pick the canonical device IDs needed for the UI
2. bootstrap current state with filtered `GET /devices`
3. normalize device payloads into local UI state
4. connect to `/events`
5. update only the affected local state when matching `device.state_changed` events arrive

This keeps the UI small, fast, and aligned with the live runtime model.

## Weather Example

The template tracks these devices:

- `open_meteo:temperature_outdoor`
- `open_meteo:wind_speed`
- `open_meteo:wind_direction`

The current device shapes are:

- `temperature_outdoor`: measurement object with `value` and `unit`
- `wind_speed`: measurement object with `value` and `unit`
- `wind_direction`: numeric integer direction in degrees

## Guidance For AI Or MCP Dashboard Generation

When generating a new dashboard against this repo:

1. treat `config/docs/api_reference.md` as the API contract
2. discover candidate device IDs from `GET /devices`
3. prefer filtered `GET /devices?ids=...` over fetching the full registry when the UI only needs a subset
4. subscribe to `/events` for live updates instead of polling
5. map canonical device attributes into a small view model before rendering
6. keep adapter-specific assumptions isolated to the chosen device IDs and attribute keys

## Extension Ideas

The same pattern can be extended to:

- room summaries
- lighting dashboards
- power and energy widgets
- media transport views
- diagnostics panels

As the UI grows, keep following the same rule:

- use the existing API contract first
- only extend the API when the new shape is broadly reusable across clients

## Reference Stack

The example uses:

- static HTML
- Alpine.js for local state and WebSocket updates
- htmx included as part of the lightweight reference stack

The initial template avoids a build system on purpose so that future agents can inspect and adapt it quickly.
