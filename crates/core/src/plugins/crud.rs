//! Generic CRUD tool definitions for plugin entities.
//!
//! These tools are registered with the agent so the AI can create, list,
//! update, delete, and search plugin data using natural language.

use crate::model::ToolDefinition;
use serde_json::json;

/// Build the standard CRUD tool definitions parameterized by available entity types.
pub fn tool_definitions(entity_types: &[String]) -> Vec<ToolDefinition> {
    let entity_enum = json!(entity_types);

    vec![
        ToolDefinition {
            name: "plugin_create".into(),
            description: "Create a new entity in a plugin's schema.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity_type": {
                        "type": "string",
                        "description": "The entity type (plugin_name/entity_name).",
                        "enum": entity_enum
                    },
                    "fields": {
                        "type": "object",
                        "description": "Field values for the new entity."
                    }
                },
                "required": ["entity_type", "fields"]
            }),
        },
        ToolDefinition {
            name: "plugin_list".into(),
            description: "List entities of a given type, with optional filters.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity_type": {
                        "type": "string",
                        "description": "The entity type (plugin_name/entity_name).",
                        "enum": entity_enum
                    },
                    "filters": {
                        "type": "object",
                        "description": "Optional field filters (field_name: value)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results to return.",
                        "default": 50
                    }
                },
                "required": ["entity_type"]
            }),
        },
        ToolDefinition {
            name: "plugin_update".into(),
            description: "Update fields on an existing entity.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity_id": {
                        "type": "string",
                        "description": "The entity's unique ID."
                    },
                    "fields": {
                        "type": "object",
                        "description": "Fields to update (field_name: new_value)."
                    }
                },
                "required": ["entity_id", "fields"]
            }),
        },
        ToolDefinition {
            name: "plugin_delete".into(),
            description: "Delete an entity by ID.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity_id": {
                        "type": "string",
                        "description": "The entity's unique ID."
                    }
                },
                "required": ["entity_id"]
            }),
        },
        ToolDefinition {
            name: "plugin_move".into(),
            description: "Move an entity to a new parent (e.g., move item to different room).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity_id": {
                        "type": "string",
                        "description": "The entity to move."
                    },
                    "new_parent_id": {
                        "type": "string",
                        "description": "The new parent entity ID."
                    }
                },
                "required": ["entity_id", "new_parent_id"]
            }),
        },
        ToolDefinition {
            name: "plugin_search".into(),
            description: "Semantic search across plugin entity data.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language search query."
                    },
                    "entity_types": {
                        "type": "array",
                        "items": { "type": "string", "enum": entity_enum },
                        "description": "Optional: limit search to specific entity types."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results.",
                        "default": 10
                    }
                },
                "required": ["query"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_six_crud_tools() {
        let types = vec!["inventory/item".into(), "inventory/room".into()];
        let tools = tool_definitions(&types);
        assert_eq!(tools.len(), 6);
        assert_eq!(tools[0].name, "plugin_create");
        assert_eq!(tools[5].name, "plugin_search");
    }
}
