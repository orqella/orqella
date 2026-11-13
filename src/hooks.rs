//! Named actions that modules can ask Orqella to perform.
//!
//! Hooks are the stable, discoverable surface for shell/daemon actions. They
//! are intentionally small and permission-gated; modules should call
//! `hooks.run` rather than depending on the internal HTTP routes.

use serde_json::{Value, json};

pub const AGENT_PROMPT: &str = "agent.prompt";
pub const CONVERSATION_OPEN: &str = "conversation.open";

pub fn list() -> Value {
    json!([
        {
            "name": AGENT_PROMPT,
            "title": "Prompt the default agent",
            "description": "Start the configured default agent and wait briefly for a conversation id.",
            "permission": "agent",
            "params": {
                "prompt": { "type": "string", "required": true },
                "root": { "type": "string", "required": true },
                "wait_ms": { "type": "number", "default": 6000, "max": 15000 },
                "open": { "type": "boolean", "default": false },
                "node": { "type": "string", "required": false }
            }
        },
        {
            "name": CONVERSATION_OPEN,
            "title": "Open a conversation",
            "description": "Open the configured conversation viewer for an agent job or session id.",
            "permission": "agent",
            "params": {
                "job": { "type": "string", "required": false },
                "session": { "type": "string", "required": false },
                "node": { "type": "string", "required": false }
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_registry_is_stable() {
        let hooks = list();
        let names = hooks
            .as_array()
            .expect("hook list")
            .iter()
            .filter_map(|hook| hook.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, vec![AGENT_PROMPT, CONVERSATION_OPEN]);
        for hook in hooks.as_array().expect("hook list") {
            assert_eq!(
                hook.get("permission").and_then(Value::as_str),
                Some("agent")
            );
            assert!(hook.get("params").and_then(Value::as_object).is_some());
        }
    }
}
