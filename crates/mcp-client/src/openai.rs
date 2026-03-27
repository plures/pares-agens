//! Convert MCP tool definitions to the OpenAI function-calling format.
//!
//! OpenAI expects tools in the form:
//! ```json
//! {
//!   "type": "function",
//!   "function": {
//!     "name": "...",
//!     "description": "...",
//!     "parameters": { "type": "object", "properties": {...}, "required": [...] }
//!   }
//! }
//! ```

use serde_json::{json, Value};

use crate::protocol::Tool;

/// Convert a single [`Tool`] into an OpenAI function-calling tool object.
pub fn to_openai_function(tool: &Tool) -> Value {
    let mut parameters: Value = json!({
        "type": tool.input_schema.schema_type.as_str(),
    });

    if let Some(props) = &tool.input_schema.properties {
        parameters["properties"] = props.clone();
    } else {
        parameters["properties"] = json!({});
    }

    if let Some(required) = &tool.input_schema.required {
        parameters["required"] = json!(required);
    }

    json!({
        "type": "function",
        "function": {
            "name": tool.name.as_str(),
            "description": tool.description.as_deref().unwrap_or(""),
            "parameters": parameters,
        }
    })
}

/// Convert a slice of [`Tool`]s into a JSON array of OpenAI function-calling
/// tool objects.
pub fn tools_to_openai(tools: &[Tool]) -> Value {
    Value::Array(tools.iter().map(to_openai_function).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Tool, ToolInputSchema};
    use serde_json::json;

    fn make_tool(name: &str, description: Option<&str>, required: Option<Vec<String>>) -> Tool {
        Tool {
            name: name.into(),
            description: description.map(Into::into),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query": { "type": "string", "description": "search query" }
                })),
                required,
            },
        }
    }

    #[test]
    fn converts_tool_with_required_fields() {
        let tool = make_tool(
            "web_search",
            Some("Search the web"),
            Some(vec!["query".into()]),
        );
        let result = to_openai_function(&tool);

        assert_eq!(result["type"], "function");
        assert_eq!(result["function"]["name"], "web_search");
        assert_eq!(result["function"]["description"], "Search the web");
        assert_eq!(result["function"]["parameters"]["type"], "object");
        assert!(result["function"]["parameters"]["properties"]["query"].is_object());
        assert_eq!(result["function"]["parameters"]["required"][0], "query");
    }

    #[test]
    fn converts_tool_without_description() {
        let tool = make_tool("ping", None, None);
        let result = to_openai_function(&tool);

        assert_eq!(result["function"]["description"], "");
        // required key should not be present
        assert!(result["function"]["parameters"].get("required").is_none());
    }

    #[test]
    fn converts_multiple_tools() {
        let tools = vec![
            make_tool("tool_a", Some("A"), None),
            make_tool("tool_b", Some("B"), None),
        ];
        let result = tools_to_openai(&tools);
        assert_eq!(result.as_array().unwrap().len(), 2);
        assert_eq!(result[0]["function"]["name"], "tool_a");
        assert_eq!(result[1]["function"]["name"], "tool_b");
    }
}
