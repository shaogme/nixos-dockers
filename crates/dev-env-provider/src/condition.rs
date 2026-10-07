use crate::context::ProviderContext;
use dev_env_model::{Condition, ConditionValue, ValueTree};

pub fn evaluate(condition: &Condition, context: &ProviderContext) -> bool {
    match condition {
        Condition::Always => true,
        Condition::Boolean(value) => *value,
        Condition::Reference(reference) => resolve_reference(reference, context)
            .map(is_truthy)
            .unwrap_or(false),
        Condition::Equal(left, right) => {
            match (resolve_value(left, context), resolve_value(right, context)) {
                (Some(left), Some(right)) => left == right,
                _ => false,
            }
        }
        Condition::NotEqual(left, right) => {
            match (resolve_value(left, context), resolve_value(right, context)) {
                (Some(left), Some(right)) => left != right,
                _ => false,
            }
        }
        Condition::And(values) => values.iter().all(|value| evaluate(value, context)),
        Condition::Or(values) => values.iter().any(|value| evaluate(value, context)),
        Condition::Not(value) => !evaluate(value, context),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ConditionRuntimeValue {
    String(String),
    Boolean(bool),
    Integer(i64),
    Other,
}

fn resolve_value(
    value: &ConditionValue,
    context: &ProviderContext,
) -> Option<ConditionRuntimeValue> {
    match value {
        ConditionValue::Boolean(value) => Some(ConditionRuntimeValue::Boolean(*value)),
        ConditionValue::Literal(value) => Some(ConditionRuntimeValue::String(value.clone())),
        ConditionValue::Reference(reference) => resolve_reference(reference, context),
    }
}

fn resolve_reference(reference: &str, context: &ProviderContext) -> Option<ConditionRuntimeValue> {
    let (namespace, path) = reference.split_once('.')?;
    match namespace {
        "provider" if path == "enabled" => {
            Some(ConditionRuntimeValue::Boolean(context.provider_enabled))
        }
        "workspace" => match path {
            "config-present" => Some(ConditionRuntimeValue::Boolean(
                context.workspace_config_present,
            )),
            "writable" => Some(ConditionRuntimeValue::Boolean(context.workspace_writable)),
            "root" => Some(ConditionRuntimeValue::String(
                context.workspace.display().to_string(),
            )),
            _ => None,
        },
        "context" => match path {
            "cwd" => Some(ConditionRuntimeValue::String(
                context.cwd.display().to_string(),
            )),
            "os" => Some(ConditionRuntimeValue::String(
                std::env::consts::OS.to_owned(),
            )),
            "arch" => Some(ConditionRuntimeValue::String(
                std::env::consts::ARCH.to_owned(),
            )),
            _ => None,
        },
        "input" | "env" => context
            .environment
            .get(path)
            .map(|value| ConditionRuntimeValue::String(value.clone())),
        "config" | "features" => context
            .namespace_value(namespace, path)
            .map(value_from_tree),
        _ => None,
    }
}

fn value_from_tree(value: &ValueTree) -> ConditionRuntimeValue {
    match value {
        ValueTree::Bool(value) => ConditionRuntimeValue::Boolean(*value),
        ValueTree::Integer(value) => ConditionRuntimeValue::Integer(*value),
        ValueTree::String(value) => ConditionRuntimeValue::String(value.clone()),
        ValueTree::Null | ValueTree::Float(_) | ValueTree::Array(_) | ValueTree::Map(_) => {
            ConditionRuntimeValue::Other
        }
    }
}

fn is_truthy(value: ConditionRuntimeValue) -> bool {
    match value {
        ConditionRuntimeValue::Boolean(value) => value,
        ConditionRuntimeValue::String(value) => !value.is_empty(),
        ConditionRuntimeValue::Integer(value) => value != 0,
        ConditionRuntimeValue::Other => false,
    }
}
