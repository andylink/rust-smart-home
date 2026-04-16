use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use mlua::{Function, Lua};
use serde::Serialize;
use smart_home_core::runtime::Runtime;
use smart_home_lua_host::{
    evaluate_module, CommandExecutionResult, LuaExecutionContext, LuaRuntimeOptions,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SceneSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SceneExecutionResult {
    pub target: String,
    pub status: &'static str,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Scene {
    pub summary: SceneSummary,
    path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct SceneCatalog {
    scenes: HashMap<String, Scene>,
    scripts_root: Option<PathBuf>,
}

impl SceneCatalog {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load_from_directory(
        path: impl AsRef<Path>,
        scripts_root: Option<PathBuf>,
    ) -> Result<Self> {
        let path = path.as_ref();
        let entries = fs::read_dir(path)
            .with_context(|| format!("failed to read scenes directory {}", path.display()))?;
        let mut scenes = HashMap::new();

        for entry in entries {
            let entry = entry.context("failed to read scenes directory entry")?;
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
            if !file_type.is_file() {
                continue;
            }

            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("lua") {
                continue;
            }

            let scene = load_scene_file(&entry.path(), scripts_root.as_deref())?;
            let scene_id = scene.summary.id.clone();
            if scenes.insert(scene_id.clone(), scene).is_some() {
                bail!("duplicate scene id '{scene_id}'");
            }
        }

        Ok(Self {
            scenes,
            scripts_root,
        })
    }

    pub fn summaries(&self) -> Vec<SceneSummary> {
        let mut scenes = self
            .scenes
            .values()
            .map(|scene| scene.summary.clone())
            .collect::<Vec<_>>();
        scenes.sort_by(|a, b| a.id.cmp(&b.id));
        scenes
    }

    pub fn execute(
        &self,
        id: &str,
        runtime: Arc<Runtime>,
    ) -> Result<Option<Vec<SceneExecutionResult>>> {
        let Some(scene) = self.scenes.get(id) else {
            return Ok(None);
        };

        let source = fs::read_to_string(&scene.path)
            .with_context(|| format!("failed to read scene file {}", scene.path.display()))?;
        let lua = Lua::new();
        let module =
            evaluate_scene_module(&lua, &source, &scene.path, self.scripts_root.as_deref())?;
        let execute = module.get::<Function>("execute").map_err(|error| {
            anyhow::anyhow!(
                "scene '{}' is missing execute function: {error}",
                scene.summary.id
            )
        })?;

        let ctx = LuaExecutionContext::new(runtime);

        execute.call::<()>(ctx.clone()).map_err(|error| {
            anyhow::anyhow!("scene '{}' execution failed: {error}", scene.summary.id)
        })?;

        Ok(Some(
            ctx.into_results()
                .into_iter()
                .map(scene_result_from_command_result)
                .collect(),
        ))
    }
}

fn load_scene_file(path: &Path, scripts_root: Option<&Path>) -> Result<Scene> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read scene file {}", path.display()))?;
    let lua = Lua::new();
    let module = evaluate_scene_module(&lua, &source, path, scripts_root)?;

    let id = module.get::<String>("id").map_err(|error| {
        anyhow::anyhow!(
            "scene file {} is missing string field 'id': {error}",
            path.display()
        )
    })?;
    let name = module.get::<String>("name").map_err(|error| {
        anyhow::anyhow!(
            "scene file {} is missing string field 'name': {error}",
            path.display()
        )
    })?;

    if id.trim().is_empty() {
        bail!("scene file {} has empty id", path.display());
    }
    if name.trim().is_empty() {
        bail!("scene file {} has empty name", path.display());
    }

    let _: Function = module.get("execute").map_err(|error| {
        anyhow::anyhow!(
            "scene file {} is missing function field 'execute': {error}",
            path.display()
        )
    })?;

    let description = module
        .get::<Option<String>>("description")
        .map_err(|error| {
            anyhow::anyhow!(
                "scene file {} has invalid optional field 'description': {error}",
                path.display()
            )
        })?;

    Ok(Scene {
        summary: SceneSummary {
            id,
            name,
            description,
        },
        path: path.to_path_buf(),
    })
}

fn evaluate_scene_module(
    lua: &Lua,
    source: &str,
    path: &Path,
    scripts_root: Option<&Path>,
) -> Result<mlua::Table> {
    evaluate_module(
        lua,
        source,
        path.to_string_lossy().as_ref(),
        &LuaRuntimeOptions {
            scripts_root: scripts_root.map(Path::to_path_buf),
        },
    )
    .map_err(|error| anyhow::anyhow!("failed to evaluate scene file {}: {error}", path.display()))
}

fn scene_result_from_command_result(result: CommandExecutionResult) -> SceneExecutionResult {
    SceneExecutionResult {
        target: result.target,
        status: result.status,
        message: result.message,
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
    use smart_home_core::model::{AttributeValue, Device, DeviceId, DeviceKind, Metadata};
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

    fn temp_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_scene(dir: &Path, name: &str, source: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, source).expect("write scene file");
        path
    }

    fn sample_device(id: &str) -> Device {
        Device {
            id: DeviceId(id.to_string()),
            room_id: None,
            kind: DeviceKind::Light,
            attributes: HashMap::from([(
                "power".to_string(),
                AttributeValue::Text("off".to_string()),
            )]),
            metadata: Metadata {
                source: "test".to_string(),
                accuracy: None,
                vendor_specific: HashMap::new(),
            },
            updated_at: chrono::Utc::now(),
            last_seen: chrono::Utc::now(),
        }
    }

    #[test]
    fn loads_valid_scene_catalog() {
        let dir = temp_dir("smart-home-scenes");
        write_scene(
            &dir,
            "video.lua",
            r#"return {
                id = "video",
                name = "Video",
                execute = function(ctx)
                end
            }"#,
        );

        let catalog = SceneCatalog::load_from_directory(&dir, None).expect("scene catalog loads");
        assert_eq!(catalog.summaries().len(), 1);
        assert_eq!(catalog.summaries()[0].id, "video");
    }

    #[test]
    fn rejects_scene_without_execute() {
        let dir = temp_dir("smart-home-scenes");
        write_scene(
            &dir,
            "broken.lua",
            r#"return {
                id = "video",
                name = "Video"
            }"#,
        );

        let error = SceneCatalog::load_from_directory(&dir, None)
            .err()
            .expect("missing execute should fail");
        assert!(error
            .to_string()
            .contains("missing function field 'execute'"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn executes_scene_commands_against_runtime() {
        let dir = temp_dir("smart-home-scenes");
        write_scene(
            &dir,
            "set-brightness.lua",
            r#"return {
                id = "set_brightness",
                name = "Set Brightness",
                execute = function(ctx)
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
                event_bus_capacity: 16,
            },
        ));
        runtime
            .registry()
            .upsert(sample_device("test:device"))
            .await
            .expect("test device exists");

        let catalog = SceneCatalog::load_from_directory(&dir, None).expect("scene catalog loads");
        let results = catalog
            .execute("set_brightness", runtime.clone())
            .expect("scene executes")
            .expect("scene exists");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "ok");
        assert_eq!(
            runtime
                .registry()
                .get(&DeviceId("test:device".to_string()))
                .expect("updated device exists")
                .attributes
                .get("brightness"),
            Some(&AttributeValue::Integer(42))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn executes_scene_using_required_script_module() {
        let dir = temp_dir("smart-home-scenes");
        let scripts_dir = temp_dir("smart-home-scripts");
        write_scene(
            &dir,
            "set-brightness.lua",
            r#"local helpers = require("lighting.helpers")

            return {
                id = "set_brightness",
                name = "Set Brightness",
                execute = function(ctx)
                    helpers.set_brightness(ctx, "test:device", 33)
                end
            }"#,
        );
        fs::create_dir_all(scripts_dir.join("lighting")).expect("create scripts namespace dir");
        fs::write(
            scripts_dir.join("lighting/helpers.lua"),
            r#"local M = {}

            function M.set_brightness(ctx, device_id, value)
                ctx:command(device_id, {
                    capability = "brightness",
                    action = "set",
                    value = value,
                })
            end

            return M"#,
        )
        .expect("write helper script");

        let runtime = Arc::new(Runtime::new(
            vec![Box::new(CommandAdapter)],
            RuntimeConfig {
                event_bus_capacity: 16,
            },
        ));
        runtime
            .registry()
            .upsert(sample_device("test:device"))
            .await
            .expect("test device exists");

        let catalog = SceneCatalog::load_from_directory(&dir, Some(scripts_dir))
            .expect("scene catalog loads");
        let results = catalog
            .execute("set_brightness", runtime.clone())
            .expect("scene executes")
            .expect("scene exists");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "ok");
        assert_eq!(
            runtime
                .registry()
                .get(&DeviceId("test:device".to_string()))
                .expect("updated device exists")
                .attributes
                .get("brightness"),
            Some(&AttributeValue::Integer(33))
        );
    }
}
