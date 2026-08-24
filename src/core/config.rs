use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::error::{Error, Result};

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub raw: Value,
    pub plugin_namespaces: BTreeMap<String, Value>,
}

impl Config {
    pub fn namespace(&self, name: &str) -> Option<&Value> {
        self.plugin_namespaces.get(name)
    }
}

pub fn aggregate(raw: Value, schemas: &[(String, Value)]) -> Result<Config> {
    let root = raw.as_object().cloned().unwrap_or_default();
    let plugins = root
        .get("plugins")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let plugins = plugins
        .as_object()
        .cloned()
        .ok_or_else(|| Error::Config("plugins must be a table/object".into()))?;
    let mut namespaces = BTreeMap::new();
    for (name, schema) in schemas {
        let value = plugins
            .get(name)
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        validate_shape(&value, schema, name)?;
        namespaces.insert(name.clone(), value);
    }
    Ok(Config {
        raw,
        plugin_namespaces: namespaces,
    })
}

fn validate_shape(value: &Value, schema: &Value, path: &str) -> Result<()> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let valid = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            _ => true,
        };
        if !valid {
            return Err(Error::Config(format!("{path} must be {expected}")));
        }
    }
    if let (Some(properties), Some(object)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) {
        for (key, child_schema) in properties {
            if let Some(child) = object.get(key) {
                validate_shape(child, child_schema, &format!("{path}.{key}"))?;
            }
        }
    }
    Ok(())
}
