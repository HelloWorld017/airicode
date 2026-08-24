use serde_json::Value;

pub fn json_schema<T>() -> Value
where
    T: schemars::JsonSchema,
{
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null)
}
