use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use mlua::{Function, Lua};
use serde::Serialize;
use smart_home_core::event::Event;
use smart_home_core::model::AttributeValue;
use smart_home_core::runtime::Runtime;
use smart_home_lua_host::{attribute_to_lua_value, evaluate_module, LuaExecutionContext};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutomationSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: &'static str,
}

#[derive(Debug, Clone)]
pub struct Automation {
    pub summary: AutomationSummary,
    path: PathBuf,
    trigger: Trigger,
}

#[derive(Debug, Clone, Default)]
pub struct AutomationCatalog {
    automations: Vec<Automation>,
}

#[derive(Debug, Clone, PartialEq)]
enum Trigger {
    DeviceStateChange {
        device_id: String,
        attribute: Option<String>,
        equals: Option<AttributeValue>,
    },
    Interval {
        every_secs: u64,
    },
}

#[derive(Debug, Clone)]
pub struct AutomationRunner {
    catalog: AutomationCatalog,
}

impl AutomationCatalog {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load_from_directory(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let entries = fs::read_dir(path)
            .with_context(|| format!("failed to read automations directory {}", path.display()))?;
        let mut automations = Vec::new();
        let mut ids = HashMap::new();

        for entry in entries {
            let entry = entry.context("failed to read automations directory entry")?;
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
            if !file_type.is_file() {
                continue;
            }

            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("lua") {
                continue;
            }

            let automation = load_automation_file(&entry.path())?;
            if ids
                .insert(automation.summary.id.clone(), automation.path.clone())
                .is_some()
            {
                bail!("duplicate automation id '{}'", automation.summary.id);
            }
            automations.push(automation);
        }

        automations.sort_by(|a, b| a.summary.id.cmp(&b.summary.id));
        Ok(Self { automations })
    }

    pub fn summaries(&self) -> Vec<AutomationSummary> {
        self.automations
            .iter()
            .map(|automation| automation.summary.clone())
            .collect()
    }
}

impl AutomationRunner {
    pub fn new(catalog: AutomationCatalog) -> Self {
        Self { catalog }
    }

    pub async fn run(self, runtime: Arc<Runtime>) {
        let mut tasks = tokio::task::JoinSet::new();

        if self
            .catalog
            .automations
            .iter()
            .any(|automation| matches!(automation.trigger, Trigger::DeviceStateChange { .. }))
        {
            let runtime = runtime.clone();
            let automations = self.catalog.automations.clone();
            tasks.spawn(async move {
                run_event_trigger_loop(runtime, automations).await;
            });
        }

        for automation in self.catalog.automations.iter().cloned() {
            if let Trigger::Interval { every_secs } = automation.trigger {
                let runtime = runtime.clone();
                tasks.spawn(async move {
                    run_interval_trigger_loop(runtime, automation, every_secs).await;
                });
            }
        }

        while tasks.join_next().await.is_some() {}
    }
}

async fn run_event_trigger_loop(runtime: Arc<Runtime>, automations: Vec<Automation>) {
    let mut receiver = runtime.bus().subscribe();

    loop {
        match receiver.recv().await {
            Ok(event) => {
                for automation in &automations {
                    if let Some(event_value) =
                        automation_event_from_runtime_event(automation, &event)
                    {
                        if let Err(error) =
                            execute_automation(automation, runtime.clone(), event_value)
                        {
                            tracing::error!(automation = %automation.summary.id, error = %error, "automation execution failed");
                        }
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "automation event trigger loop lagged behind");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn run_interval_trigger_loop(runtime: Arc<Runtime>, automation: Automation, every_secs: u64) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(every_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let event = AttributeValue::Object(HashMap::from([
            (
                "type".to_string(),
                AttributeValue::Text("interval".to_string()),
            ),
            (
                "scheduled_at".to_string(),
                AttributeValue::Text(Utc::now().to_rfc3339()),
            ),
            (
                "every_secs".to_string(),
                AttributeValue::Integer(every_secs as i64),
            ),
        ]));

        if let Err(error) = execute_automation(&automation, runtime.clone(), event) {
            tracing::error!(automation = %automation.summary.id, error = %error, "interval automation execution failed");
        }
    }
}

fn execute_automation(
    automation: &Automation,
    runtime: Arc<Runtime>,
    event: AttributeValue,
) -> Result<()> {
    let source = fs::read_to_string(&automation.path).with_context(|| {
        format!(
            "failed to read automation file {}",
            automation.path.display()
        )
    })?;
    let lua = Lua::new();
    let module = evaluate_automation_module(&lua, &source, &automation.path)?;
    let execute = module.get::<Function>("execute").map_err(|error| {
        anyhow::anyhow!(
            "automation '{}' is missing execute function: {error}",
            automation.summary.id
        )
    })?;

    let ctx = LuaExecutionContext::new(runtime);
    let event =
        attribute_to_lua_value(&lua, event).map_err(|error| anyhow::anyhow!(error.to_string()))?;

    execute.call::<()>((ctx, event)).map_err(|error| {
        anyhow::anyhow!(
            "automation '{}' execution failed: {error}",
            automation.summary.id
        )
    })
}

fn automation_event_from_runtime_event(
    automation: &Automation,
    event: &Event,
) -> Option<AttributeValue> {
    let Trigger::DeviceStateChange {
        device_id,
        attribute,
        equals,
    } = &automation.trigger
    else {
        return None;
    };

    let Event::DeviceStateChanged { id, attributes } = event else {
        return None;
    };

    if &id.0 != device_id {
        return None;
    }

    if let Some(attribute_name) = attribute {
        let value = attributes.get(attribute_name)?;
        if let Some(expected) = equals {
            if value != expected {
                return None;
            }
        }

        return Some(AttributeValue::Object(HashMap::from([
            (
                "type".to_string(),
                AttributeValue::Text("device_state_change".to_string()),
            ),
            ("device_id".to_string(), AttributeValue::Text(id.0.clone())),
            (
                "attribute".to_string(),
                AttributeValue::Text(attribute_name.clone()),
            ),
            ("value".to_string(), value.clone()),
            (
                "attributes".to_string(),
                AttributeValue::Object(attributes.clone()),
            ),
        ])));
    }

    Some(AttributeValue::Object(HashMap::from([
        (
            "type".to_string(),
            AttributeValue::Text("device_state_change".to_string()),
        ),
        ("device_id".to_string(), AttributeValue::Text(id.0.clone())),
        (
            "attributes".to_string(),
            AttributeValue::Object(attributes.clone()),
        ),
    ])))
}

fn load_automation_file(path: &Path) -> Result<Automation> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read automation file {}", path.display()))?;
    let lua = Lua::new();
    let module = evaluate_automation_module(&lua, &source, path)?;

    let id = module.get::<String>("id").map_err(|error| {
        anyhow::anyhow!(
            "automation file {} is missing string field 'id': {error}",
            path.display()
        )
    })?;
    let name = module.get::<String>("name").map_err(|error| {
        anyhow::anyhow!(
            "automation file {} is missing string field 'name': {error}",
            path.display()
        )
    })?;
    let trigger_value = module.get::<mlua::Value>("trigger").map_err(|error| {
        anyhow::anyhow!(
            "automation file {} is missing field 'trigger': {error}",
            path.display()
        )
    })?;
    let trigger = parse_trigger(trigger_value, path)?;

    if id.trim().is_empty() {
        bail!("automation file {} has empty id", path.display());
    }
    if name.trim().is_empty() {
        bail!("automation file {} has empty name", path.display());
    }

    let _: Function = module.get("execute").map_err(|error| {
        anyhow::anyhow!(
            "automation file {} is missing function field 'execute': {error}",
            path.display()
        )
    })?;

    let description = module
        .get::<Option<String>>("description")
        .map_err(|error| {
            anyhow::anyhow!(
                "automation file {} has invalid optional field 'description': {error}",
                path.display()
            )
        })?;

    Ok(Automation {
        summary: AutomationSummary {
            id,
            name,
            description,
            trigger_type: trigger_type_name(&trigger),
        },
        path: path.to_path_buf(),
        trigger,
    })
}

fn evaluate_automation_module(lua: &Lua, source: &str, path: &Path) -> Result<mlua::Table> {
    evaluate_module(lua, source, path.to_string_lossy().as_ref()).map_err(|error| {
        anyhow::anyhow!(
            "failed to evaluate automation file {}: {error}",
            path.display()
        )
    })
}

fn parse_trigger(value: mlua::Value, path: &Path) -> Result<Trigger> {
    let mlua::Value::Table(table) = value else {
        bail!(
            "automation file {} field 'trigger' must be a table",
            path.display()
        );
    };

    let trigger_type = table.get::<String>("type").map_err(|error| {
        anyhow::anyhow!(
            "automation file {} trigger is missing string field 'type': {error}",
            path.display()
        )
    })?;

    match trigger_type.as_str() {
        "device_state_change" => {
            let device_id = table.get::<String>("device_id").map_err(|error| {
                anyhow::anyhow!(
                    "automation file {} device_state_change trigger requires 'device_id': {error}",
                    path.display()
                )
            })?;
            let attribute = table.get::<Option<String>>("attribute").map_err(|error| {
                anyhow::anyhow!(
                    "automation file {} trigger field 'attribute' is invalid: {error}",
                    path.display()
                )
            })?;
            let equals = match table.get::<mlua::Value>("equals") {
                Ok(mlua::Value::Nil) => None,
                Ok(value) => Some(
                    smart_home_lua_host::lua_value_to_attribute(value)
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?,
                ),
                Err(error) => {
                    return Err(anyhow::anyhow!(
                        "automation file {} trigger field 'equals' is invalid: {error}",
                        path.display()
                    ))
                }
            };

            Ok(Trigger::DeviceStateChange {
                device_id,
                attribute,
                equals,
            })
        }
        "interval" => {
            let every_secs = table.get::<u64>("every_secs").map_err(|error| {
                anyhow::anyhow!(
                    "automation file {} interval trigger requires positive 'every_secs': {error}",
                    path.display()
                )
            })?;
            if every_secs == 0 {
                bail!("automation file {} interval trigger 'every_secs' must be > 0", path.display());
            }

            Ok(Trigger::Interval { every_secs })
        }
        _ => bail!(
            "automation file {} has unsupported trigger type '{}'; supported types are device_state_change and interval",
            path.display(),
            trigger_type
        ),
    }
}

fn trigger_type_name(trigger: &Trigger) -> &'static str {
    match trigger {
        Trigger::DeviceStateChange { .. } => "device_state_change",
        Trigger::Interval { .. } => "interval",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use anyhow::Result;
    use smart_home_core::adapter::Adapter;
    use smart_home_core::bus::EventBus;
    use smart_home_core::command::DeviceCommand;
    use smart_home_core::model::{Device, DeviceId, DeviceKind, Metadata};
    use smart_home_core::registry::DeviceRegistry;
    use smart_home_core::runtime::{Runtime, RuntimeConfig};

    use super::*;

    struct CommandAdapter;

    #[async_trait::async_trait]
    impl Adapter for CommandAdapter {
        fn name(&self) -> &str {
            "test"
        }

        async fn run(&self, _registry: DeviceRegistry, _bus: EventBus) -> Result<()> {
            std::future::pending::<()>().await;
            Ok(())
        }

        async fn command(
            &self,
            device_id: &DeviceId,
            command: DeviceCommand,
            registry: DeviceRegistry,
        ) -> Result<bool> {
            if device_id.0 != "test:device" {
                return Ok(false);
            }

            let mut device = registry.get(device_id).expect("test device exists");
            device.attributes.insert(
                command.capability,
                command.value.expect("test command must include value"),
            );
            registry
                .upsert(device)
                .await
                .expect("registry update succeeds");
            Ok(true)
        }
    }

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("smart-home-automations-{unique}"));
        fs::create_dir_all(&path).expect("create temp automations dir");
        path
    }

    fn write_automation(dir: &Path, name: &str, source: &str) {
        fs::write(dir.join(name), source).expect("write automation file");
    }

    fn sample_device(id: &str, wet: bool) -> Device {
        Device {
            id: DeviceId(id.to_string()),
            room_id: None,
            kind: DeviceKind::Sensor,
            attributes: HashMap::from([("rain".to_string(), AttributeValue::Bool(wet))]),
            metadata: Metadata {
                source: "test".to_string(),
                accuracy: None,
                vendor_specific: HashMap::new(),
            },
            updated_at: Utc::now(),
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn loads_device_trigger_automation_catalog() {
        let dir = temp_dir();
        write_automation(
            &dir,
            "rain.lua",
            r#"return {
                id = "rain_check",
                name = "Rain Check",
                trigger = {
                    type = "device_state_change",
                    device_id = "test:rain",
                    attribute = "rain",
                    equals = true,
                },
                execute = function(ctx, event)
                end
            }"#,
        );

        let catalog = AutomationCatalog::load_from_directory(&dir).expect("catalog loads");
        assert_eq!(catalog.summaries().len(), 1);
        assert_eq!(catalog.summaries()[0].trigger_type, "device_state_change");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn event_automation_executes_on_matching_state_change() {
        let dir = temp_dir();
        write_automation(
            &dir,
            "rain.lua",
            r#"return {
                id = "rain_check",
                name = "Rain Check",
                trigger = {
                    type = "device_state_change",
                    device_id = "test:rain",
                    attribute = "rain",
                    equals = true,
                },
                execute = function(ctx, event)
                    ctx:command("test:device", {
                        capability = "brightness",
                        action = "set",
                        value = 42,
                    })
                end
            }"#,
        );

        let runtime = Arc::new(Runtime::new(
            vec![Box::new(CommandAdapter)],
            RuntimeConfig {
                event_bus_capacity: 32,
            },
        ));
        runtime
            .registry()
            .upsert(sample_device("test:rain", false))
            .await
            .expect("sensor upsert succeeds");
        runtime
            .registry()
            .upsert(Device {
                id: DeviceId("test:device".to_string()),
                room_id: None,
                kind: DeviceKind::Light,
                attributes: HashMap::from([("brightness".to_string(), AttributeValue::Integer(0))]),
                metadata: Metadata {
                    source: "test".to_string(),
                    accuracy: None,
                    vendor_specific: HashMap::new(),
                },
                updated_at: Utc::now(),
                last_seen: Utc::now(),
            })
            .await
            .expect("target device upsert succeeds");

        let catalog = AutomationCatalog::load_from_directory(&dir).expect("catalog loads");
        let runner = AutomationRunner::new(catalog);
        let runtime_for_runner = runtime.clone();
        let task = tokio::spawn(async move {
            runner.run(runtime_for_runner).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;

        runtime
            .registry()
            .upsert(sample_device("test:rain", true))
            .await
            .expect("sensor change succeeds");

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        assert_eq!(
            runtime
                .registry()
                .get(&DeviceId("test:device".to_string()))
                .expect("target device exists")
                .attributes
                .get("brightness"),
            Some(&AttributeValue::Integer(42))
        );

        task.abort();
        let _ = task.await;
    }
}
