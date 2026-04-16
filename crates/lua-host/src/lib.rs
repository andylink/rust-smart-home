use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result};
use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use smart_home_core::command::DeviceCommand;
use smart_home_core::invoke::InvokeRequest;
use smart_home_core::model::{AttributeValue, DeviceId};
use smart_home_core::runtime::Runtime;
use tokio::runtime::Handle;
use tokio::task::block_in_place;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandExecutionResult {
    pub target: String,
    pub status: &'static str,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LuaRuntimeOptions {
    pub scripts_root: Option<PathBuf>,
}

#[derive(Clone)]
pub struct LuaExecutionContext {
    runtime: Arc<Runtime>,
    execution_results: ExecutionResults,
}

#[derive(Clone, Default)]
struct ExecutionResults(Rc<RefCell<Vec<CommandExecutionResult>>>);

#[derive(Clone)]
struct ScriptLoader {
    root: PathBuf,
}

impl ExecutionResults {
    fn push(&self, result: CommandExecutionResult) {
        self.0.borrow_mut().push(result);
    }

    fn take(&self) -> Vec<CommandExecutionResult> {
        self.0.borrow().clone()
    }
}

impl LuaExecutionContext {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self {
            runtime,
            execution_results: ExecutionResults::default(),
        }
    }

    pub fn into_results(self) -> Vec<CommandExecutionResult> {
        self.execution_results.take()
    }
}

impl UserData for LuaExecutionContext {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method(
            "command",
            |_, this, (device_id, command): (String, Table)| {
                let command = lua_table_to_command(&command)?;
                command.validate().map_err(mlua::Error::external)?;

                let result = match block_in_place(|| {
                    Handle::current().block_on(
                        this.runtime
                            .command_device(&DeviceId(device_id.clone()), command),
                    )
                }) {
                    Ok(true) => CommandExecutionResult {
                        target: device_id,
                        status: "ok",
                        message: None,
                    },
                    Ok(false) => CommandExecutionResult {
                        target: device_id,
                        status: "unsupported",
                        message: Some("device commands are not implemented".to_string()),
                    },
                    Err(error) => CommandExecutionResult {
                        target: device_id,
                        status: "error",
                        message: Some(error.to_string()),
                    },
                };
                this.execution_results.push(result);

                Ok(())
            },
        );

        methods.add_method("invoke", |lua, this, (target, payload): (String, Value)| {
            let payload = lua_value_to_attribute(payload)?;
            let response = block_in_place(|| {
                Handle::current().block_on(this.runtime.invoke(InvokeRequest {
                    target: target.clone(),
                    payload,
                }))
            })
            .map_err(mlua::Error::external)?
            .ok_or_else(|| {
                mlua::Error::external(format!("invoke target '{target}' is not supported"))
            })?;

            attribute_to_lua_value(&lua, response.value)
        });
    }
}

impl ScriptLoader {
    fn install(&self, lua: &Lua) -> Result<()> {
        let package: Table = lua
            .globals()
            .get("package")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let loader = self.clone();

        let searcher = lua
            .create_function(move |lua, module_name: String| {
                match loader.load_module(lua, &module_name) {
                    Ok(value) => {
                        let loaded = value;
                        let module_loader = lua.create_function(move |_, ()| Ok(loaded.clone()))?;
                        Ok(Value::Function(module_loader))
                    }
                    Err(error) => Ok(Value::String(lua.create_string(&error.to_string())?)),
                }
            })
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        let searchers: Table = package
            .get::<Table>("searchers")
            .or_else(|_| package.get::<Table>("loaders"))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        let len = searchers.raw_len();
        for index in (2..=len).rev() {
            let value: Value = searchers
                .raw_get(index)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            searchers
                .raw_set(index + 1, value)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        searchers
            .raw_set(2, searcher)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        Ok(())
    }

    fn load_module(&self, lua: &Lua, module_name: &str) -> mlua::Result<Value> {
        let module_path =
            resolve_script_module_path(&self.root, module_name).map_err(mlua::Error::external)?;
        let source = fs::read_to_string(&module_path).map_err(mlua::Error::external)?;
        let chunk_name = format!("@{}", module_path.display());

        lua.load(&source)
            .set_name(&chunk_name)
            .eval::<Value>()
            .map_err(|error| {
                mlua::Error::external(format!(
                    "failed to load script module '{}': {}",
                    module_name, error
                ))
            })
    }
}

pub fn prepare_lua(lua: &Lua, options: &LuaRuntimeOptions) -> Result<()> {
    if let Some(root) = &options.scripts_root {
        ScriptLoader { root: root.clone() }.install(lua)?;
    }

    Ok(())
}

pub fn evaluate_module(
    lua: &Lua,
    source: &str,
    path_name: &str,
    options: &LuaRuntimeOptions,
) -> Result<Table> {
    prepare_lua(lua, options)?;

    let value = lua
        .load(source)
        .set_name(path_name)
        .eval::<Value>()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    match value {
        Value::Table(table) => Ok(table),
        _ => anyhow::bail!("lua module must return a table"),
    }
}

pub fn lua_table_to_command(table: &Table) -> mlua::Result<DeviceCommand> {
    Ok(DeviceCommand {
        capability: table.get("capability")?,
        action: table.get("action")?,
        value: match table.get::<Value>("value")? {
            Value::Nil => None,
            value => Some(lua_value_to_attribute(value)?),
        },
    })
}

pub fn lua_value_to_attribute(value: Value) -> mlua::Result<AttributeValue> {
    match value {
        Value::Nil => Ok(AttributeValue::Null),
        Value::Boolean(value) => Ok(AttributeValue::Bool(value)),
        Value::Integer(value) => Ok(AttributeValue::Integer(value)),
        Value::Number(value) => Ok(AttributeValue::Float(value)),
        Value::String(value) => Ok(AttributeValue::Text(value.to_str()?.to_string())),
        Value::Table(table) => {
            if is_array_table(&table)? {
                let mut values = Vec::new();
                for value in table.sequence_values::<Value>() {
                    values.push(lua_value_to_attribute(value?)?);
                }
                return Ok(AttributeValue::Array(values));
            }

            let mut fields = HashMap::new();
            for pair in table.pairs::<Value, Value>() {
                let (key, value) = pair?;
                let Value::String(key) = key else {
                    return Err(mlua::Error::external("lua object keys must be strings"));
                };
                fields.insert(key.to_str()?.to_string(), lua_value_to_attribute(value)?);
            }
            Ok(AttributeValue::Object(fields))
        }
        _ => Err(mlua::Error::external(
            "lua values must be nil, boolean, number, string, or table",
        )),
    }
}

pub fn attribute_to_lua_value(lua: &Lua, value: AttributeValue) -> mlua::Result<Value> {
    match value {
        AttributeValue::Null => Ok(Value::Nil),
        AttributeValue::Bool(value) => Ok(Value::Boolean(value)),
        AttributeValue::Integer(value) => Ok(Value::Integer(value)),
        AttributeValue::Float(value) => Ok(Value::Number(value)),
        AttributeValue::Text(value) => Ok(Value::String(lua.create_string(&value)?)),
        AttributeValue::Array(values) => {
            let table = lua.create_table()?;
            for (index, value) in values.into_iter().enumerate() {
                table.set(index + 1, attribute_to_lua_value(lua, value)?)?;
            }
            Ok(Value::Table(table))
        }
        AttributeValue::Object(fields) => {
            let table = lua.create_table()?;
            for (key, value) in fields {
                table.set(key, attribute_to_lua_value(lua, value)?)?;
            }
            Ok(Value::Table(table))
        }
    }
}

fn is_array_table(table: &Table) -> mlua::Result<bool> {
    let mut count = 0usize;

    for pair in table.pairs::<Value, Value>() {
        let (key, _) = pair?;
        let Value::Integer(index) = key else {
            return Ok(false);
        };
        if index <= 0 {
            return Ok(false);
        }
        count += 1;
    }

    for expected in 1..=count {
        if table.raw_get::<Value>(expected as i64)? == Value::Nil {
            return Ok(false);
        }
    }

    Ok(true)
}

fn resolve_script_module_path(root: &Path, module_name: &str) -> Result<PathBuf> {
    if module_name.trim().is_empty() {
        anyhow::bail!("script module name must not be empty");
    }

    let mut path = root.to_path_buf();
    for part in module_name.split('.') {
        if part.is_empty() {
            anyhow::bail!("script module name '{module_name}' is invalid");
        }

        let component_path = Path::new(part);
        if component_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            anyhow::bail!("script module name '{module_name}' is invalid");
        }

        path.push(part);
    }
    path.set_extension("lua");

    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("failed to access scripts directory {}", root.display()))?;
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("script module '{}' was not found", module_name))?;

    if !canonical_path.starts_with(&canonical_root) {
        anyhow::bail!(
            "script module '{}' is outside the scripts directory",
            module_name
        );
    }

    Ok(canonical_path)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("smart-home-lua-host-{unique}"));
        fs::create_dir_all(&path).expect("create temp scripts dir");
        path
    }

    #[test]
    fn resolves_namespaced_script_module_paths() {
        let root = temp_dir();
        fs::create_dir_all(root.join("vision")).expect("create nested dir");
        fs::write(root.join("vision/ollama.lua"), "return {} ").expect("write script");

        let resolved = resolve_script_module_path(&root, "vision.ollama").expect("resolve path");
        assert!(resolved.ends_with("vision/ollama.lua"));
    }
}
