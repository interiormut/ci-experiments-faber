//! Three independent hashes over the out-of-band head of a request — the
//! parts that sit ahead of the messages array and invalidate the whole
//! cached prefix when they drift (`proposal.md` §5.2). Kept as three hashes
//! rather than one so a `scaffold_mismatch` names which part changed
//! (§11.5).
//!
//! Hashed with `DefaultHasher` rather than anything cryptographic: this
//! fingerprint is compared only within one process and never persisted, so
//! collision resistance across runs or machines was never a requirement.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::canonical::canonical_json;
use crate::state::ScaffoldPrint;

/// Order-sensitive: a reorder of an otherwise-identical tool array *is* a
/// cache miss (`abstract.md` H5's tool-def note), so it must hash different.
pub fn tools_hash(tools: &[llm::ToolDef]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for tool in tools {
        let value = serde_json::json!({
            "name": tool.name,
            "description": tool.description,
            "inputSchema": tool.input_schema,
        });
        canonical_json(&value).hash(&mut hasher);
    }
    hasher.finish()
}

/// `None` when there is no leading system run to hash — nothing to compare a
/// later request's head against yet.
pub fn system_hash<'a>(head: impl Iterator<Item = &'a llm::Message>) -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    let mut any = false;
    for message in head {
        any = true;
        let value = serde_json::to_value(message).expect("Message always serializes");
        canonical_json(&value).hash(&mut hasher);
    }
    any.then(|| hasher.finish())
}

pub fn model_hash(provider: &str, model: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    provider.hash(&mut hasher);
    model.hash(&mut hasher);
    hasher.finish()
}

/// Builds the full fingerprint for `(tools, lineage, provider, model)`.
/// `lineage` is the *whole* lineage, not just its head — the leading system
/// run is found here via [`llm::leading_system_run`], the same rule
/// `crates/llm`'s Anthropic renderer uses, so this never drifts from what
/// actually gets hoisted onto the wire.
pub fn print_for<'a>(
    tools: &[llm::ToolDef],
    lineage: impl Iterator<Item = &'a llm::Message> + Clone,
    provider: &str,
    model: &str,
) -> ScaffoldPrint {
    let head_len = llm::leading_system_run(lineage.clone());
    ScaffoldPrint {
        tools: tools_hash(tools),
        system: system_hash(lineage.take(head_len)),
        model: model_hash(provider, model),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reordered_tool_array_hashes_differently() {
        let a = llm::ToolDef {
            name: "a".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        };
        let b = llm::ToolDef {
            name: "b".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        };
        assert_ne!(tools_hash(&[a.clone(), b.clone()]), tools_hash(&[b, a]));
    }

    #[test]
    fn no_leading_system_turn_hashes_to_none() {
        let messages = [llm::Message::user("hi")];
        let head = &messages[..llm::leading_system_run(&messages)];
        assert_eq!(system_hash(head.iter()), None);
    }

    #[test]
    fn the_model_component_is_sensitive_to_both_provider_and_model() {
        assert_ne!(
            model_hash("anthropic", "claude"),
            model_hash("openai", "claude")
        );
        assert_ne!(
            model_hash("anthropic", "claude"),
            model_hash("anthropic", "gpt")
        );
    }
}
