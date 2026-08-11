//! JSON Schema auto-generation for tool parameters via [`schemars`].

use schemars::JsonSchema;
use serde_json::Value;

/// Generate an OpenAI-compatible JSON Schema from a type implementing [`JsonSchema`].
///
/// Post-processing:
/// 1. Strips `"$schema"` — not part of the OpenAI tool schema contract.
/// 2. Sets `"additionalProperties": false` — prevents hallucinated parameters.
pub fn generate_schema<T: JsonSchema>() -> Value {
    let root_schema = schemars::schema_for!(T);
    let mut value = serde_json::to_value(&root_schema)
        .expect("schemars RootSchema must always serialize to JSON");

    if let Value::Object(map) = &mut value {
        map.remove("$schema");
        map.entry("additionalProperties")
            .or_insert(Value::Bool(false));
    }

    value
}

#[cfg(test)]
mod tests {
    use super::*;

    use schemars::JsonSchema;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct SimpleArgs {
        #[schemars(description = "A required string.")]
        text: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct OptionalArgs {
        #[schemars(description = "Required.")]
        file_path: String,
        #[schemars(description = "Optional.")]
        offset: Option<u64>,
    }

    #[test]
    fn generated_schema_has_type_object() {
        let schema = generate_schema::<SimpleArgs>();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn generated_schema_has_additional_properties_false() {
        let schema = generate_schema::<SimpleArgs>();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn generated_schema_strips_dollar_schema() {
        let schema = generate_schema::<SimpleArgs>();
        assert!(schema.get("$schema").is_none());
    }

    #[test]
    fn optional_fields_not_in_required() {
        let schema = generate_schema::<OptionalArgs>();
        let required = &schema["required"];
        let required_names: Vec<&str> = required
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required_names.contains(&"file_path"));
        assert!(!required_names.contains(&"offset"));
    }
}
