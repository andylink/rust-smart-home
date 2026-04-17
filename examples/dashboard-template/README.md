# Dashboard Template

This folder contains a tiny external dashboard reference for the Smart Home API.

It is intentionally outside `crates/api` so the API stays API-only.

## What It Shows

- one read-only weather card
- initial state loaded from filtered `GET /devices`
- live updates from WebSocket `/events`
- a minimal pattern that AI or MCP tooling can extend into larger dashboards

Tracked device IDs:

- `open_meteo:temperature_outdoor`
- `open_meteo:wind_speed`
- `open_meteo:wind_direction`

## Run It

Start the API first:

```bash
cargo run -p api -- --config config/default.toml
```

Before loading the dashboard from another origin such as `http://127.0.0.1:8080`, enable CORS for that origin in `config/default.toml`:

```toml
[api.cors]
enabled = true
allowed_origins = ["http://127.0.0.1:8080"]
```

Serve this directory as static files from the repo root:

```bash
python -m http.server 8080
```

Then open:

```text
http://127.0.0.1:8080/examples/dashboard-template/?api=http://127.0.0.1:3001
```

If you open `index.html` directly from disk, browser security rules may block API requests or WebSocket behavior.
Serving it over HTTP is the intended workflow.

## API Contract Used

Initial data load:

```text
GET /devices?ids=open_meteo:temperature_outdoor&ids=open_meteo:wind_speed&ids=open_meteo:wind_direction
```

Live updates:

```text
GET /events
```

The template listens for `device.state_changed` events and updates the weather card when one of the tracked IDs changes.

## Notes

- `htmx` is included as part of the reference stack, but Alpine handles the live state updates here
- this example has no build tooling and no npm dependency
- the API base can be changed in the page or via the `?api=` query parameter
- the API must allow the dashboard origin through `api.cors.allowed_origins`

## Extend It

To add another card:

1. add more device IDs to the filtered `GET /devices` request
2. map those canonical device records into local UI state
3. update the WebSocket handler to watch the extra IDs
4. render a new card from the normalized state

For more guidance, see `config/docs/dashboard_template_guide.md`.
