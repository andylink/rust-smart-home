use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
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

#[derive(Clone)]
pub struct LuaExecutionContext {
    runtime: Arc<Runtime>,
    execution_results: ExecutionResults,
}

#[derive(Clone, Default)]
struct ExecutionResults(Rc<RefCell<Vec<CommandExecutionResult>>>);

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

pub fn evaluate_module(lua: &Lua, source: &str, path_name: &str) -> Result<Table> {
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
