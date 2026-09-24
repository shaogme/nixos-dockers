use crate::context::RuntimeContext;
use crate::error::CoreError;
use bootstrap_model::{BootstrapConfig, BootstrapInput, InputNamespace, InputValue, ParsedInput};
use container_init_posix::{PosixSystem, WorkspaceObservation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const DEFAULT_ROOT_HOME: &str = "/root";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    RunAsRoot,
    ExplicitHost,
    ExplicitContainer,
    WorkspaceMount,
    ProfileDefault,
    Current,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Mounted,
    NotMounted,
    Unavailable,
}

impl WorkspaceStatus {
    fn from_observation(observation: &WorkspaceObservation) -> Self {
        match observation {
            WorkspaceObservation::Mounted { .. } => Self::Mounted,
            WorkspaceObservation::NotMounted { .. } => Self::NotMounted,
            WorkspaceObservation::Unavailable { .. } => Self::Unavailable,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedIdentity {
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
    pub run_as_root: bool,
    pub uid_source: IdentitySource,
    pub gid_source: IdentitySource,
    pub workspace: WorkspaceStatus,
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
        self.resolve_prevalidated(config, context)
    }

    pub fn resolve_prevalidated(
        &self,
        config: &BootstrapConfig,
        context: &RuntimeContext,
    ) -> Result<ResolvedIdentity, CoreError> {
        let inputs = self.resolve_inputs(config, context)?;
        let run_as_root = input_bool(
            config,
            &inputs,
            config.identity.run_as_root_input.as_deref(),
        )?
        .unwrap_or(false);

        let configured_home = config.identity.default_home.as_deref();

        if run_as_root {
            let home = input_home(config, &inputs, config.identity.home_input.as_deref())?
                .or_else(|| configured_home.map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT_HOME));
            return Ok(ResolvedIdentity {
                uid: 0,
                gid: 0,
                user: "root".to_owned(),
                home,
                run_as_root: true,
                uid_source: IdentitySource::RunAsRoot,
                gid_source: IdentitySource::RunAsRoot,
                workspace: WorkspaceStatus::NotMounted,
            });
        }

        let uid_pair = input_uid_pair(config, &inputs, config.identity.uid_input.as_deref())?;
        let explicit_gid = input_gid(config, &inputs, config.identity.gid_input.as_deref())?;
        let configured_user = config.identity.default_user.clone();
        if configured_user.as_deref() == Some("root") {
            let requested_uid = uid_pair.as_ref().map(|pair| pair.uid);
            if requested_uid
                .or(config.identity.default_uid)
                .is_some_and(|uid| uid != 0)
            {
                return Err(CoreError::Identity {
                    message: "default_user=\"root\" cannot be mapped to a non-zero UID".to_owned(),
                });
            }
        }
        let named_user = configured_user
            .as_deref()
            .map(|name| self.posix.lookup_user_by_name(name))
            .transpose()
            .map_err(|source| CoreError::io(None, None, source))?
            .flatten();

        let observation = context
            .workspace_observation()
            .cloned()
            .unwrap_or_else(|| self.posix.observe_workspace(context.cwd()));
        let workspace_status = WorkspaceStatus::from_observation(&observation);
        let mounted_owner = match &observation {
            WorkspaceObservation::Mounted { uid, gid, .. } => Some((*uid, *gid)),
            WorkspaceObservation::NotMounted { .. } | WorkspaceObservation::Unavailable { .. } => {
                None
            }
        };

        let (pair, pair_namespace) = match uid_pair {
            Some(pair) => {
                let namespace =
                    input_namespace(config, &inputs, config.identity.uid_input.as_deref())?;
                let uid = map_uid(&self.posix, pair.uid, namespace)?;
                let gid = pair
                    .gid
                    .map(|gid| map_gid(&self.posix, gid, namespace))
                    .transpose()?;
                (Some(UidPair { uid, gid }), Some(namespace))
            }
            None => (None, None),
        };

        let (uid, uid_source) = if let Some(pair) = pair {
            (
                pair.uid,
                source_for_namespace(pair_namespace.expect("pair namespace")),
            )
        } else if config.identity.auto_mapping {
            if let Some((uid, _)) = mounted_owner {
                (uid, IdentitySource::WorkspaceMount)
            } else if let Some(uid) = config.identity.default_uid {
                (uid, IdentitySource::ProfileDefault)
            } else if let Some(user) = &named_user {
                (user.uid, IdentitySource::ProfileDefault)
            } else {
                (self.posix.current_ids().0, IdentitySource::Current)
            }
        } else if let Some(uid) = config.identity.default_uid {
            (uid, IdentitySource::ProfileDefault)
        } else if let Some(user) = &named_user {
            (user.uid, IdentitySource::ProfileDefault)
        } else {
            (self.posix.current_ids().0, IdentitySource::Current)
        };

        let (gid, gid_source) = if let Some(gid) = explicit_gid {
            let namespace = input_namespace(config, &inputs, config.identity.gid_input.as_deref())?;
            (
                map_gid(&self.posix, gid, namespace)?,
                source_for_namespace(namespace),
            )
        } else if let Some(gid) = pair.and_then(|pair| pair.gid) {
            (
                gid,
                source_for_namespace(pair_namespace.expect("pair namespace")),
            )
        } else if config.identity.auto_mapping {
            if let Some((_, gid)) = mounted_owner {
                (gid, IdentitySource::WorkspaceMount)
            } else if let Some(gid) = config.identity.default_gid {
                (gid, IdentitySource::ProfileDefault)
            } else if let Some(user) = &named_user {
                (user.gid, IdentitySource::ProfileDefault)
            } else {
                (self.posix.current_ids().1, IdentitySource::Current)
            }
        } else if let Some(gid) = config.identity.default_gid {
            (gid, IdentitySource::ProfileDefault)
        } else if let Some(user) = &named_user {
            (user.gid, IdentitySource::ProfileDefault)
        } else {
            (uid, IdentitySource::Current)
        };

        let uid_user = self
            .posix
            .lookup_user_by_uid(uid)
            .map_err(|source| CoreError::io(None, None, source))?;
        let user = if uid == 0 {
            if uid_user
                .as_ref()
                .is_some_and(|candidate| candidate.name != "root")
            {
                return Err(CoreError::Identity {
                    message: format!(
                        "UID 0 is already occupied by passwd user {:?}, not root",
                        uid_user.as_ref().expect("checked above").name
                    ),
                });
            }
            "root".to_owned()
        } else if let Some(user) = configured_user {
            if user == "root" {
                return Err(CoreError::Identity {
                    message: "non-zero UID cannot use the root account name".to_owned(),
                });
            }
            user
        } else if let Some(user) = &uid_user {
            user.name.clone()
        } else {
            format!("uid-{uid}")
        };

        if uid != 0 {
            if let Some(owner) = uid_user.as_ref() {
                if owner.name != user {
                    return Err(CoreError::Identity {
                        message: format!(
                            "target UID {uid} is already occupied by passwd user {:?}",
                            owner.name
                        ),
                    });
                }
            }
        }

        let home = input_home(config, &inputs, config.identity.home_input.as_deref())?
            .or_else(|| configured_home.map(PathBuf::from))
            .or_else(|| {
                if uid == 0 {
                    Some(PathBuf::from(DEFAULT_ROOT_HOME))
                } else {
                    named_user
                        .as_ref()
                        .map(|candidate| candidate.home.clone())
                        .or_else(|| uid_user.as_ref().map(|candidate| candidate.home.clone()))
                        .or_else(|| Some(PathBuf::from(format!("/home/{user}"))))
                }
            })
            .expect("identity home fallback always produces a path");

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
            uid_source,
            gid_source,
            workspace: workspace_status,
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
        .iter()
        .find(|(_, declaration)| declaration.aliases.iter().any(|alias| alias == name))
        .and_then(|(canonical, _)| inputs.get(canonical))
}

fn input_declaration<'a>(
    config: &'a BootstrapConfig,
    name: Option<&str>,
) -> Option<&'a BootstrapInput> {
    let name = name?;
    config.inputs.get(name).or_else(|| {
        config
            .inputs
            .values()
            .find(|declaration| declaration.aliases.iter().any(|alias| alias == name))
    })
}

fn input_namespace(
    config: &BootstrapConfig,
    inputs: &ResolvedInputs,
    name: Option<&str>,
) -> Result<InputNamespace, CoreError> {
    let Some(input) = find_input(config, inputs, name) else {
        return Err(CoreError::Identity {
            message: "configured UID/GID input is not set".to_owned(),
        });
    };
    input_declaration(config, Some(&input.name))
        .and_then(|declaration| declaration.namespace)
        .ok_or_else(|| CoreError::Identity {
            message: format!("input {} has no declared namespace", input.name),
        })
}

fn map_uid(posix: &PosixSystem, uid: u32, namespace: InputNamespace) -> Result<u32, CoreError> {
    match namespace {
        InputNamespace::Host => {
            posix
                .map_uid_from_parent(uid)
                .map_err(|error| CoreError::Identity {
                    message: format!("cannot map host UID {uid}: {error}"),
                })
        }
        InputNamespace::Container => Ok(uid),
    }
}

fn map_gid(posix: &PosixSystem, gid: u32, namespace: InputNamespace) -> Result<u32, CoreError> {
    match namespace {
        InputNamespace::Host => {
            posix
                .map_gid_from_parent(gid)
                .map_err(|error| CoreError::Identity {
                    message: format!("cannot map host GID {gid}: {error}"),
                })
        }
        InputNamespace::Container => Ok(gid),
    }
}

fn source_for_namespace(namespace: InputNamespace) -> IdentitySource {
    match namespace {
        InputNamespace::Host => IdentitySource::ExplicitHost,
        InputNamespace::Container => IdentitySource::ExplicitContainer,
    }
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
    let declaration = input_declaration(config, Some(&input.name));
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
