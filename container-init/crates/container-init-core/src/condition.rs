use crate::context::RuntimeContext;
use crate::error::CoreError;
use crate::identity::{ResolvedIdentity, ResolvedInputs};
use bootstrap_model::{Condition, ConditionValue, PathTemplate};
use container_init_posix::is_writable;
use std::collections::BTreeMap;
use std::path::Path;

/// Evaluation context for the deliberately small Bootstrap condition language.
pub struct ConditionContext<'a> {
    identity: &'a ResolvedIdentity,
    inputs: &'a ResolvedInputs,
    runtime: &'a RuntimeContext,
    workspace_root: &'a Path,
}

impl<'a> ConditionContext<'a> {
    pub fn new(
        identity: &'a ResolvedIdentity,
        inputs: &'a ResolvedInputs,
        runtime: &'a RuntimeContext,
        workspace_root: &'a Path,
    ) -> Self {
        Self {
            identity,
            inputs,
            runtime,
            workspace_root,
        }
    }

    pub fn evaluate(&self, condition: &Condition) -> Result<bool, CoreError> {
        match condition {
            Condition::Always | Condition::Boolean(true) | Condition::ContextPathExistsOrCreate => {
                Ok(true)
            }
            Condition::Boolean(false) => Ok(false),
            Condition::Exists(path) => Ok(self.render(path)?.exists()),
            Condition::Writable(path) => Ok(is_writable(&self.render(path)?)),
            Condition::InputSet(name) => Ok(self.inputs.contains(name)),
            Condition::Feature(feature) => Ok(self.runtime.has_feature(feature)),
            Condition::Equal(left, right) => Ok(self.value(left)? == self.value(right)?),
            Condition::NotEqual(left, right) => Ok(self.value(left)? != self.value(right)?),
            Condition::And(conditions) => {
                for condition in conditions {
                    if !self.evaluate(condition)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Condition::Or(conditions) => {
                for condition in conditions {
                    if self.evaluate(condition)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Condition::Not(condition) => Ok(!self.evaluate(condition)?),
        }
    }

    fn render(&self, template: &PathTemplate) -> Result<std::path::PathBuf, CoreError> {
        let mut values = BTreeMap::new();
        values.insert(
            "bootstrap.workspace_root".to_owned(),
            self.workspace_root.to_string_lossy().into_owned(),
        );
        values.insert("identity.uid".to_owned(), self.identity.uid.to_string());
        values.insert("identity.gid".to_owned(), self.identity.gid.to_string());
        values.insert("identity.user".to_owned(), self.identity.user.clone());
        values.insert(
            "identity.home".to_owned(),
            self.identity.home.to_string_lossy().into_owned(),
        );
        values.insert("identity.target".to_owned(), self.identity.user.clone());
        values.insert(
            "context.cwd".to_owned(),
            self.runtime.cwd().to_string_lossy().into_owned(),
        );
        values.insert("context.os".to_owned(), std::env::consts::OS.to_owned());
        values.insert("context.arch".to_owned(), std::env::consts::ARCH.to_owned());
        for (name, input) in self.inputs.iter() {
            values.insert(format!("input.{name}"), input.raw.clone());
        }
        for (name, value) in self.runtime.environment() {
            values.insert(format!("env.{name}"), value.clone());
        }
        template
            .render(&values)
            .map(std::path::PathBuf::from)
            .map_err(|message| CoreError::Invalid {
                location: "bootstrap condition".to_owned(),
                message,
            })
    }

    fn value(&self, value: &ConditionValue) -> Result<ComparableValue, CoreError> {
        match value {
            ConditionValue::Literal(value) => Ok(ComparableValue::String(value.clone())),
            ConditionValue::Boolean(value) => Ok(ComparableValue::Bool(*value)),
            ConditionValue::Reference(reference) => match reference.as_str() {
                "identity.uid" => Ok(ComparableValue::String(self.identity.uid.to_string())),
                "identity.gid" => Ok(ComparableValue::String(self.identity.gid.to_string())),
                "identity.user" => Ok(ComparableValue::String(self.identity.user.clone())),
                "identity.home" => Ok(ComparableValue::String(
                    self.identity.home.to_string_lossy().into_owned(),
                )),
                "identity.target" => Ok(ComparableValue::String(self.identity.user.clone())),
                "context.cwd" => Ok(ComparableValue::String(
                    self.runtime.cwd().to_string_lossy().into_owned(),
                )),
                "context.os" => Ok(ComparableValue::String(std::env::consts::OS.to_owned())),
                "context.arch" => Ok(ComparableValue::String(std::env::consts::ARCH.to_owned())),
                reference if reference.starts_with("input.") => self
                    .inputs
                    .value(&reference["input.".len()..])
                    .map(|value| ComparableValue::String(value.to_owned()))
                    .ok_or_else(|| CoreError::Invalid {
                        location: "bootstrap condition".to_owned(),
                        message: format!("runtime input {reference:?} is not set"),
                    }),
                reference if reference.starts_with("env.") => self
                    .runtime
                    .env(&reference["env.".len()..])
                    .map(|value| ComparableValue::String(value.to_owned()))
                    .ok_or_else(|| CoreError::Invalid {
                        location: "bootstrap condition".to_owned(),
                        message: format!("ambient environment value {reference:?} is not set"),
                    }),
                reference => Err(CoreError::Invalid {
                    location: "bootstrap condition".to_owned(),
                    message: format!(
                        "reference {reference:?} cannot be evaluated by container-init"
                    ),
                }),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ComparableValue {
    String(String),
    Bool(bool),
}
