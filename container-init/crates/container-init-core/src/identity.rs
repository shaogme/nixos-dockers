use crate::context::RuntimeContext;
use crate::error::CoreError;
use bootstrap_model::{BootstrapConfig, BootstrapInput, InputValue, ParsedInput};
use container_init_posix::PosixSystem;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedIdentity {
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
    pub run_as_root: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeInput {
    pub name: String,
    pub raw: String,
    pub parsed: ParsedRuntimeInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedRuntimeInput {
    UidPair { uid: u32, gid: Option<u32> },
    Gid(u32),
    Bool(bool),
    Path(PathBuf),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedInputs {
    values: BTreeMap<String, RuntimeInput>,
}

impl ResolvedInputs {
    pub fn get(&self, name: &str) -> Option<&RuntimeInput> {
        self.values.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &RuntimeInput)> {
        self.values
            .iter()
            .map(|(name, input)| (name.as_str(), input))
    }

    pub(crate) fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(|input| input.raw.as_str())
    }
}

#[derive(Clone, Debug, Default)]
pub struct IdentityResolver {
    posix: PosixSystem,
}

impl IdentityResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_posix(posix: PosixSystem) -> Self {
        Self { posix }
    }

    pub fn resolve_inputs(
        &self,
        config: &BootstrapConfig,
        context: &RuntimeContext,
    ) -> Result<ResolvedInputs, CoreError> {
        let mut values = BTreeMap::new();
        for (name, declaration) in &config.inputs {
            let Some(raw) = declared_value(name, declaration, context) else {
                continue;
            };
            let parsed = declaration.parse_value(&raw).map_err(CoreError::Model)?;
            let parsed = to_runtime_value(parsed);
            let input = RuntimeInput {
                name: name.clone(),
                raw,
                parsed,
            };
            values.insert(name.clone(), input.clone());
            for alias in &declaration.aliases {
                values.insert(alias.clone(), input.clone());
            }
        }
        Ok(ResolvedInputs { values })
    }

    pub fn resolve(
        &self,
        config: &BootstrapConfig,
        context: &RuntimeContext,
    ) -> Result<ResolvedIdentity, CoreError> {
        config.validate().map_err(CoreError::Model)?;
        let inputs = self.resolve_inputs(config, context)?;
        let run_as_root = input_bool(
            config,
            &inputs,
            config.identity.run_as_root_input.as_deref(),
        )?
        .unwrap_or(false);

        if run_as_root {
            let home = input_home(config, &inputs, config.identity.home_input.as_deref())?
                .unwrap_or_else(|| PathBuf::from("/root"));
            return Ok(ResolvedIdentity {
                uid: 0,
                gid: 0,
                user: "root".to_owned(),
                home,
                run_as_root: true,
            });
        }

        let uid_pair = input_uid_pair(config, &inputs, config.identity.uid_input.as_deref())?;
        let explicit_gid = input_gid(config, &inputs, config.identity.gid_input.as_deref())?;
        let configured_user = config.identity.default_user.clone();
        let named_user = configured_user
            .as_deref()
            .map(|name| self.posix.lookup_user_by_name(name))
            .transpose()
            .map_err(|source| CoreError::io(None, None, source))?
            .flatten();
        let workspace_owner = context
            .workspace_owner()
            .or_else(|| probe_workspace_owner(&config.workspace_root));

        // The order here mirrors the Bootstrap design: declared runtime input,
        // profile mapping, workspace ownership, and finally profile/current
        // defaults. A HOST_UID pair may carry its own GID.
        let (uid, workspace_gid) = if let Some(pair) = uid_pair {
            (pair.uid, pair.gid)
        } else if config.identity.auto_mapping {
            if let Some((uid, gid)) = workspace_owner {
                (uid, Some(gid))
            } else if let Some(uid) = config.identity.default_uid {
                (uid, None)
            } else if let Some(user) = &named_user {
                (user.uid, Some(user.gid))
            } else {
                let (uid, gid) = self.posix.current_ids();
                (uid, Some(gid))
            }
        } else if let Some(uid) = config.identity.default_uid {
            (uid, None)
        } else if let Some(user) = &named_user {
            (user.uid, Some(user.gid))
        } else {
            let (uid, gid) = self.posix.current_ids();
            (uid, Some(gid))
        };
        let gid = explicit_gid
            .or(workspace_gid)
            .or(config.identity.default_gid)
            .or_else(|| named_user.as_ref().map(|user| user.gid))
            .unwrap_or(uid);

        let user = configured_user
            .or_else(|| {
                self.posix
                    .lookup_user_by_uid(uid)
                    .ok()
                    .flatten()
                    .map(|user| user.name)
            })
            .unwrap_or_else(|| {
                if uid == 0 {
                    "root".to_owned()
                } else {
                    format!("uid-{uid}")
                }
            });

        let home = input_home(config, &inputs, config.identity.home_input.as_deref())?
            .or_else(|| named_user.as_ref().map(|user| user.home.clone()))
            .or_else(|| {
                self.posix
                    .lookup_user_by_name(&user)
                    .ok()
                    .flatten()
                    .map(|user| user.home)
            })
            .unwrap_or_else(|| {
                if uid == 0 {
                    PathBuf::from("/root")
                } else {
                    PathBuf::from(format!("/home/{user}"))
                }
            });

        if !home.is_absolute() {
            return Err(CoreError::Identity {
                message: format!("resolved HOME {} is not absolute", home.display()),
            });
        }
        Ok(ResolvedIdentity {
            uid,
            gid,
            user,
            home,
            run_as_root: false,
        })
    }
}

fn declared_value(
    name: &str,
    declaration: &BootstrapInput,
    context: &RuntimeContext,
) -> Option<String> {
    if !declaration.runtime {
        return declaration.default.as_ref().map(input_value_string);
    }
    let mut names = Vec::with_capacity(1 + declaration.aliases.len());
    names.push(name);
    names.extend(declaration.aliases.iter().map(String::as_str));
    names
        .iter()
        .find_map(|name| context.cli_input(name))
        .or_else(|| names.iter().find_map(|name| context.env(name)))
        .map(str::to_owned)
        .or_else(|| declaration.default.as_ref().map(input_value_string))
}

fn input_value_string(value: &InputValue) -> String {
    match value {
        InputValue::Bool(value) => value.to_string(),
        InputValue::Integer(value) => value.to_string(),
        InputValue::String(value) => value.clone(),
    }
}

fn to_runtime_value(value: ParsedInput) -> ParsedRuntimeInput {
    match value {
        ParsedInput::UidPair { uid, gid } => ParsedRuntimeInput::UidPair { uid, gid },
        ParsedInput::Gid(gid) => ParsedRuntimeInput::Gid(gid),
        ParsedInput::Bool(value) => ParsedRuntimeInput::Bool(value),
        ParsedInput::Path(value) => ParsedRuntimeInput::Path(PathBuf::from(value)),
    }
}

fn find_input<'a>(
    config: &BootstrapConfig,
    inputs: &'a ResolvedInputs,
    name: Option<&str>,
) -> Option<&'a RuntimeInput> {
    let name = name?;
    if let Some(input) = inputs.get(name) {
        return Some(input);
    }
    config
        .inputs
        .values()
        .find(|declaration| declaration.aliases.iter().any(|alias| alias == name))
        .and_then(|declaration| {
            inputs.get(&declaration.target).or_else(|| {
                config
                    .inputs
                    .iter()
                    .find(|(_, candidate)| *candidate == declaration)
                    .and_then(|(canonical, _)| inputs.get(canonical))
            })
        })
}

fn input_uid_pair(
    config: &BootstrapConfig,
    inputs: &ResolvedInputs,
    name: Option<&str>,
) -> Result<Option<UidPair>, CoreError> {
    let Some(input) = find_input(config, inputs, name) else {
        return Ok(None);
    };
    match input.parsed {
        ParsedRuntimeInput::UidPair { uid, gid } => Ok(Some(UidPair { uid, gid })),
        _ => Err(CoreError::Identity {
            message: format!("input {} is not a uid_pair", input.name),
        }),
    }
}

fn input_gid(
    config: &BootstrapConfig,
    inputs: &ResolvedInputs,
    name: Option<&str>,
) -> Result<Option<u32>, CoreError> {
    let Some(input) = find_input(config, inputs, name) else {
        return Ok(None);
    };
    match input.parsed {
        ParsedRuntimeInput::Gid(gid) => Ok(Some(gid)),
        _ => Err(CoreError::Identity {
            message: format!("input {} is not a gid", input.name),
        }),
    }
}

fn input_bool(
    config: &BootstrapConfig,
    inputs: &ResolvedInputs,
    name: Option<&str>,
) -> Result<Option<bool>, CoreError> {
    let Some(input) = find_input(config, inputs, name) else {
        return Ok(None);
    };
    match input.parsed {
        ParsedRuntimeInput::Bool(value) => Ok(Some(value)),
        _ => Err(CoreError::Identity {
            message: format!("input {} is not a bool", input.name),
        }),
    }
}

fn input_home(
    config: &BootstrapConfig,
    inputs: &ResolvedInputs,
    name: Option<&str>,
) -> Result<Option<PathBuf>, CoreError> {
    let Some(input) = find_input(config, inputs, name) else {
        return Ok(None);
    };
    let ParsedRuntimeInput::Path(path) = &input.parsed else {
        return Err(CoreError::Identity {
            message: format!("input {} is not a path", input.name),
        });
    };
    let declaration = config.inputs.get(&input.name).or_else(|| {
        config
            .inputs
            .values()
            .find(|candidate| candidate.aliases.iter().any(|alias| alias == &input.name))
    });
    if declaration.is_some_and(|declaration| !declaration.allow_outside_workspace)
        && !path.starts_with(&config.workspace_root)
    {
        return Err(CoreError::Identity {
            message: format!(
                "input {} resolves outside the configured workspace",
                input.name
            ),
        });
    }
    Ok(Some(path.clone()))
}

#[derive(Clone, Copy)]
struct UidPair {
    uid: u32,
    gid: Option<u32>,
}

fn probe_workspace_owner(path: &str) -> Option<(u32, u32)> {
    fs::metadata(path).ok().map(|metadata| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            (metadata.uid(), metadata.gid())
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            (0, 0)
        }
    })
}
