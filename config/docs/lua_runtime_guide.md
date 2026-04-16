# Lua Runtime Guide

This document describes the current Lua runtime surface for scenes and automations.

It supersedes the older scene-only implementation plan in `lua_scenes_implementation_plan.md` for day-to-day usage.

## Purpose

Lua assets are the main user-authored orchestration layer.

Current Lua asset roots:

- `config/scenes/`
- `config/automations/`

Reserved for future work:

- `config/scripts/`

## Current Rust Layout

- `crates/lua-host/`
  - shared Lua execution context
  - `ctx:command(...)`
  - `ctx:invoke(...)`
  - Lua <-> `AttributeValue` conversion
- `crates/scenes/`
  - scene catalog and scene execution
- `crates/automations/`
  - automation catalog
  - trigger matching
  - automation runner

This keeps the Lua host logic shared and avoids duplicating scripting behavior across scenes and automations.

## Shared Host API

Both scenes and automations currently receive the same host API through `ctx`.

Available methods:

- `ctx:command(device_id, command_table)`
- `ctx:invoke(target, payload_table)`

### `ctx:command(...)`

`command_table` maps to the canonical Rust `DeviceCommand` shape:

- `capability`
- `action`
- optional `value`

Example:

```lua
ctx:command("elgato_lights:light:0", {
  capability = "brightness",
  action = "set",
  value = 50,
})
```

Execution behavior:

- command validation uses the existing canonical Rust path
- dispatch goes through `Runtime::command_device()`
- per-command results are recorded for scene execution responses

### `ctx:invoke(...)`

`ctx:invoke(target, payload_table)` dispatches a service-style request to the adapter that owns the target prefix.

Example:

```lua
local result = ctx:invoke("ollama:chat", {
  messages = {
    {
      role = "user",
      content = "Give me a short home summary.",
    },
  },
})

local reply = result.message.content
```

## Scenes

Scenes are manual, user-invoked actions.

Location:

- `config/scenes/*.lua`

Current contract:

```lua
return {
  id = "video",
  name = "Video",
  description = "Prepare devices for a video call",
  execute = function(ctx)
    ctx:command("roku_tv:tv", {
      capability = "power",
      action = "off",
    })

    ctx:command("elgato_lights:light:0", {
      capability = "power",
      action = "on",
    })
  end
}
```

Required fields:

- `id`
- `name`
- `execute`

Optional fields:

- `description`

Notes:

- scenes are loaded at API startup
- duplicate scene IDs fail startup
- scene files must return a Lua table

## Automations

Automations are trigger-driven Lua assets.

Location:

- `config/automations/*.lua`

Current contract:

```lua
return {
  id = "rain_check",
  name = "Rain Check",
  trigger = {
    type = "device_state_change",
    device_id = "weather:outside",
    attribute = "rain",
    equals = true,
  },
  execute = function(ctx, event)
    local result = ctx:invoke("ollama:vision", {
      prompt = "Reply only true or false. Are clothes on the clothesline?",
      image_base64 = "BASE64_IMAGE_HERE",
    })

    if result.boolean == true then
      ctx:command("elgato_lights:light:0", {
        capability = "power",
        action = "on",
      })
    end
  end,
}
```

Required fields:

- `id`
- `name`
- `trigger`
- `execute`

Optional fields:

- `description`

### Trigger Types

Current first-pass trigger types:

- `device_state_change`
- `interval`

#### `device_state_change`

Fields:

- `device_id` required
- `attribute` optional
- `equals` optional

Example:

```lua
trigger = {
  type = "device_state_change",
  device_id = "weather:outside",
  attribute = "rain",
  equals = true,
}
```

Behavior:

- listens for `Event::DeviceStateChanged`
- matches the target device ID
- if `attribute` is present, requires that attribute to be present in the changed attribute set
- if `equals` is also present, the attribute value must match exactly

Event object passed to `execute(ctx, event)` includes:

- `event.type`
- `event.device_id`
- `event.attribute` when filtered by attribute
- `event.value` when filtered by attribute
- `event.attributes`

#### `interval`

Fields:

- `every_secs` required and must be greater than zero

Example:

```lua
trigger = {
  type = "interval",
  every_secs = 3600,
}
```

Event object passed to `execute(ctx, event)` includes:

- `event.type = "interval"`
- `event.scheduled_at`
- `event.every_secs`

## Ollama Integration Through Lua

Ollama is exposed through `ctx:invoke(...)` and the `adapter-ollama` crate.

Current built-in targets:

- `ollama:generate`
- `ollama:vision`
- `ollama:chat`
- `ollama:embeddings`
- `ollama:tags`
- `ollama:ps`
- `ollama:show`
- `ollama:version`

Examples:

```lua
local result = ctx:invoke("ollama:vision", {
  prompt = "Reply only true or false. Are clothes on the clothesline?",
  image_base64 = snapshot_base64,
})
```

```lua
local result = ctx:invoke("ollama:chat", {
  messages = {
    {
      role = "system",
      content = "Be concise.",
    },
    {
      role = "user",
      content = "Summarize the weather in one sentence.",
    },
  },
})
```

## Configuration

Current config sections:

```toml
[scenes]
enabled = true
directory = "config/scenes"

[automations]
enabled = true
directory = "config/automations"
```

## Current Limitations

Not implemented yet:

- `config/scripts/` module loading
- hot reload for scenes or automations
- cron or wall-clock scheduling beyond `interval`
- room-scoped Lua helpers
- direct device read helpers on `ctx`
- explicit Lua logging helpers

## Recommended Usage Split

- scenes: manual, user-invoked flows
- automations: trigger-driven decisions
- adapters: external system integrations
- future scripts: reusable Lua helpers
