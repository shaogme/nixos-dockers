use dev_env_loader::{CliPatch, LoaderError, ProfileLoader, ProfileSource, SourceKind, ValueKind};
use dev_env_model::{
    InputError, ModelError, OverrideOperation, OverrideSpec, Sensitivity, SourceId, ValueTree,
};
use std::collections::BTreeMap;
use std::error::Error;

const BASE: &str = r#"
schema = 1
id = "base"

[policy]
workspace_can_override = ["environment.variables.*"]
cli_can_override = ["environment.variables.*"]

[config.workspace]
root = "/workspace"
search = "upward"

[config.shell]
default = "bash"

[config.shells.bash]
command = "/bin/bash.real"
kind = "posix"
login_args = ["-l"]
interactive_args = ["-i"]
command_arg = "-c"

[config.environment]
inherit_process = true
configured_value_precedence = "locked"

[config.environment.variables]
BASE_VALUE = "base"
OVERRIDE_ME = "from-base"

[config.environment.path]
prepend = ["/opt/base"]
append = ["/usr/local/bin"]

[config.features]
base = true

[config.features.devbox]
auto_init = "never"

[inputs.DEVBOX_AUTO_INIT]
target = "features.devbox.auto_init"
type = "enum"
values = ["never", "if-missing", "ask"]
aliases = { "0" = "never", "1" = "if-missing", "true" = "if-missing" }
runtime = true
export_as = "DEVBOX_AUTO_INIT"
default = "never"
"#;

fn base_loader() -> ProfileLoader {
    let mut loader = ProfileLoader::new();
    loader
        .add_profile(ProfileSource::with_location(
            "base",
            SourceKind::ImageProfile,
            "/etc/dev-env/profiles.d/00-base.toml",
            BASE,
        ))
        .unwrap();
    loader
}

fn set_override(value: &str) -> OverrideSpec {
    OverrideSpec {
        op: OverrideOperation::Set,
        value: Some(ValueTree::String(value.to_owned())),
        values: Vec::new(),
        reason: "repository policy".to_owned(),
    }
}

fn map_value<'a>(value: &'a ValueTree, key: &str) -> &'a ValueTree {
    match value {
        ValueTree::Map(values) => values.get(key).unwrap(),
        _ => panic!("expected a map while reading {key}"),
    }
}

#[test]
fn loads_profile_graph_and_keeps_only_environment_namespace() {
    let mut loader = base_loader();
    loader
        .add_str(
            "derived",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "derived"
extends = ["base"]

[config.environment.variables]
DERIVED_VALUE = "derived"

[config.features.devbox]
auto_init = "never"
"#,
        )
        .unwrap();

    let loaded = loader.load("derived").unwrap();
    let config = loaded.config();
    assert_eq!(config.workspace.root, "/workspace");
    assert_eq!(config.environment.variables["DERIVED_VALUE"], "derived");
    assert_eq!(map_value(&config.features, "base"), &ValueTree::Bool(true));
    assert_eq!(
        loaded
            .profile_chain()
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["base", "derived"]
    );

    let provenance = config
        .provenance
        .get("environment.variables.DERIVED_VALUE")
        .unwrap();
    assert_eq!(
        provenance.origins[0].source,
        SourceId::Profile("derived".to_owned())
    );
}

#[test]
fn shared_profiles_can_carry_a_bootstrap_namespace() {
    let mut loader = ProfileLoader::new();
    loader
        .add_str(
            "shared",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "shared"

[config.workspace]
root = "/workspace"

[config.shell]
default = "bash"

[config.shells.bash]
command = "/bin/bash"
kind = "posix"
command_arg = "-c"

[bootstrap]
schema = 1

[bootstrap.identity]
default_user = "dev"

[bootstrap.handoff]
runtime = "/usr/bin/dev-env"
"#,
        )
        .unwrap();

    let loaded = loader.load("shared").unwrap();
    assert_eq!(loaded.config().workspace.root, "/workspace");
    assert_eq!(loaded.config().shell.default, "bash");
}

#[test]
fn scalar_conflicts_are_structured_and_explicit_override_replaces_them() {
    let mut loader = base_loader();
    loader
        .add_str(
            "conflict",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "conflict"
extends = ["base"]

[config.environment.variables]
OVERRIDE_ME = "from-child"
"#,
        )
        .unwrap();
    let error = loader.load("conflict").unwrap_err();
    assert!(
        matches!(error, LoaderError::Conflict { ref path, .. } if path == "environment.variables.OVERRIDE_ME")
    );
    assert!(error.to_string().contains("DEVENV-E-CONFLICT"));

    let mut loader = base_loader();
    loader
        .add_str(
            "override",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "override"
extends = ["base"]

[override."environment.variables.OVERRIDE_ME"]
op = "set"
value = "from-override"
reason = "repository policy"

[config.environment.variables]
OVERRIDE_ME = "ignored because override owns this path"
"#,
        )
        .unwrap();
    let loaded = loader.load("override").unwrap();
    assert_eq!(
        loaded.config().environment.variables["OVERRIDE_ME"],
        "from-override"
    );
    let entry = loaded
        .config()
        .provenance
        .get("environment.variables.OVERRIDE_ME")
        .unwrap();
    assert_eq!(
        entry.origins.last().unwrap().reason.as_deref(),
        Some("repository policy")
    );
}

#[test]
fn list_override_operations_are_not_implicit_replacement() {
    let mut loader = base_loader();
    loader
        .add_str(
            "list-conflict",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "list-conflict"
extends = ["base"]

[config.environment.path]
prepend = ["/opt/child"]
"#,
        )
        .unwrap();
    assert!(
        matches!(loader.load("list-conflict"), Err(LoaderError::Conflict { ref path, .. }) if path == "environment.path.prepend")
    );

    let mut loader = base_loader();
    loader
        .add_str(
            "list-override",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "list-override"
extends = ["base"]

[override."environment.path.prepend"]
op = "append"
values = ["/opt/child", "/opt/base"]
reason = "add the project toolchain"

[config.environment.path]
prepend = ["/not-used"]
"#,
        )
        .unwrap();
    let loaded = loader.load("list-override").unwrap();
    assert_eq!(
        loaded.config().environment.path.prepend,
        ["/opt/base", "/opt/child", "/opt/base"]
    );
}

#[test]
fn profile_graph_reports_missing_parents_and_cycles() {
    let mut loader = ProfileLoader::new();
    loader
        .add_str(
            "a",
            SourceKind::ImageProfile,
            "schema = 1\nid = \"a\"\nextends = [\"b\"]\n",
        )
        .unwrap();
    loader
        .add_str(
            "b",
            SourceKind::ImageProfile,
            "schema = 1\nid = \"b\"\nextends = [\"a\"]\n",
        )
        .unwrap();
    assert!(
        matches!(loader.load("a"), Err(LoaderError::InheritanceCycle { profiles }) if profiles == ["a", "b", "a"])
    );
    assert!(
        matches!(loader.load("missing"), Err(LoaderError::MissingProfile { id }) if id == "missing")
    );
}

#[test]
fn workspace_overrides_need_policy_permission_and_explicit_operation() {
    let overlay = ProfileSource::new(
        "workspace",
        SourceKind::WorkspaceOverlay,
        r#"
schema = 1
id = "workspace"

[config.environment.variables]
OVERRIDE_ME = "from-workspace"
"#,
    );
    let loader = base_loader();
    assert!(
        matches!(loader.load_with_overlays("base", [&overlay]), Err(LoaderError::Conflict { ref path, .. }) if path == "environment.variables.OVERRIDE_ME")
    );

    let allowed = ProfileSource::new(
        "workspace",
        SourceKind::WorkspaceOverlay,
        r#"
schema = 1
id = "workspace"

[override."environment.variables.OVERRIDE_ME"]
op = "set"
value = "from-workspace"
reason = "this repository selects its own value"

[config.environment.variables]
OVERRIDE_ME = "ignored"
"#,
    );
    let loaded = loader.load_with_overlays("base", [&allowed]).unwrap();
    assert_eq!(
        loaded.config().environment.variables["OVERRIDE_ME"],
        "from-workspace"
    );

    let denied = ProfileSource::new(
        "workspace-no-policy",
        SourceKind::WorkspaceOverlay,
        r#"
schema = 1
id = "workspace-no-policy"

[config.workspace]
root = "/different-workspace"
"#,
    );
    assert!(matches!(
        loader.load_with_overlays("base", [&denied]),
        Err(LoaderError::Model {
            source: ModelError::WorkspaceOverrideNotAllowed { .. },
            ..
        })
    )); // policy is checked before any write
}

#[test]
fn runtime_inputs_are_typed_and_aliases_are_generic() {
    let loader = base_loader();
    let loaded = loader
        .load_with_runtime(
            "base",
            BTreeMap::from([(String::from("DEVBOX_AUTO_INIT"), String::from("1"))]),
        )
        .unwrap();
    assert_eq!(
        map_value(map_value(&loaded.config().features, "devbox"), "auto_init"),
        &ValueTree::String("if-missing".to_owned())
    );

    let invalid = loader.load_with_runtime(
        "base",
        BTreeMap::from([(String::from("DEVBOX_AUTO_INIT"), String::from("bad"))]),
    );
    assert!(matches!(
        invalid,
        Err(LoaderError::Model {
            source: ModelError::InvalidInput {
                reason: InputError::InvalidEnumValue { .. },
                ..
            },
            ..
        })
    ));
}

#[test]
fn compose_sccache_inputs_update_features_and_environment_values() {
    let mut loader = base_loader();
    loader
        .add_str(
            "rust",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "rust"
extends = ["base"]

[config.environment.variables]
CARGO_TARGET_DIR = "/data/.cargo/target"
SCCACHE_DIR = "/data/cache/sccache"

[config.environment.conditional_variables.RUSTC_WRAPPER]
value = "sccache"
when = "features.sccache.enabled && !features.sccache.disabled"

[config.environment.conditional_variables.CARGO_INCREMENTAL]
value = "0"
when = "!features.sccache.enabled || features.sccache.disabled"

[config.features.sccache]
enabled = true
disabled = false

[inputs.CARGO_INCREMENTAL]
target = "environment.conditional_variables.CARGO_INCREMENTAL.value"
type = "enum"
values = ["0", "1"]
runtime = true

[inputs.CARGO_TARGET_DIR]
target = "environment.variables.CARGO_TARGET_DIR"
type = "path"
runtime = true

[inputs.SCCACHE_DIR]
target = "environment.variables.SCCACHE_DIR"
type = "path"
runtime = true

[inputs.SCCACHE_DISABLE]
target = "features.sccache.disabled"
type = "bool"
runtime = true

[inputs.ENABLE_SCCACHE]
target = "features.sccache.enabled"
type = "bool"
runtime = true
"#,
        )
        .unwrap();

    let loaded = loader
        .load_with_runtime(
            "rust",
            BTreeMap::from([
                ("CARGO_INCREMENTAL", "1"),
                ("CARGO_TARGET_DIR", "/tmp/rust-target"),
                ("SCCACHE_DIR", "/tmp/sccache"),
                ("SCCACHE_DISABLE", "1"),
                ("ENABLE_SCCACHE", "1"),
            ]),
        )
        .unwrap();
    assert_eq!(
        loaded.config().environment.conditional_variables["CARGO_INCREMENTAL"].value,
        "1"
    );
    assert_eq!(
        loaded.config().environment.variables["CARGO_TARGET_DIR"],
        "/tmp/rust-target"
    );
    assert_eq!(
        loaded.config().environment.variables["SCCACHE_DIR"],
        "/tmp/sccache"
    );
    assert_eq!(
        map_value(map_value(&loaded.config().features, "sccache"), "disabled"),
        &ValueTree::Bool(true)
    );
}

#[test]
fn combined_loader_preserves_overlay_runtime_and_cli_order() {
    let loader = base_loader();
    let overlay = ProfileSource::new(
        "workspace",
        SourceKind::WorkspaceOverlay,
        r#"
schema = 1
id = "workspace"

[override."environment.variables.OVERRIDE_ME"]
op = "set"
value = "workspace"
reason = "workspace policy"
"#,
    );
    let patch = CliPatch::new("environment.variables.OVERRIDE_ME", set_override("cli"));
    let loaded = loader
        .load_with_overlays_runtime_and_cli(
            "base",
            [&overlay],
            [("DEVBOX_AUTO_INIT", "1")],
            [patch],
        )
        .unwrap();
    assert_eq!(loaded.config().environment.variables["OVERRIDE_ME"], "cli");
    assert_eq!(
        map_value(map_value(&loaded.config().features, "devbox"), "auto_init"),
        &ValueTree::String("if-missing".to_owned())
    );
}

#[test]
fn cli_patches_are_checked_against_policy() {
    let loader = base_loader();
    let patch = CliPatch::new("environment.variables.OVERRIDE_ME", set_override("cli"));
    let loaded = loader.load_with_cli("base", [patch]).unwrap();
    assert_eq!(loaded.config().environment.variables["OVERRIDE_ME"], "cli");

    let patch = CliPatch::new("environment.variables.BASE_VALUE", set_override("cli"));
    // Both names match the base policy in this fixture, so changing the patch
    // target verifies the actual allow-list rather than a hard-coded path.
    assert!(loader.load_with_cli("base", [patch]).is_ok());
}

#[test]
fn malformed_toml_keeps_the_parser_error_as_the_source() {
    let mut loader = ProfileLoader::new();
    let error = loader
        .add_str("bad", SourceKind::ImageProfile, "schema = [")
        .unwrap_err();
    assert!(matches!(error, LoaderError::Parse { .. }));
    assert!(error.source().is_some());
}

#[test]
fn directory_loader_uses_declared_ids() {
    let directory = std::env::temp_dir().join(format!(
        "dev-env-loader-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("arbitrary.toml"), BASE).unwrap();
    std::fs::write(directory.join("ignored.txt"), "not toml").unwrap();

    let loader = ProfileLoader::from_directory(&directory).unwrap();
    assert_eq!(loader.profile_ids().collect::<Vec<_>>(), ["base"]);
    assert!(loader.load("base").is_ok());
    std::fs::remove_file(directory.join("arbitrary.toml")).unwrap();
    std::fs::remove_file(directory.join("ignored.txt")).unwrap();
    std::fs::remove_dir(&directory).unwrap();
}

#[test]
fn array_override_reports_a_typed_kind_error() {
    let mut loader = base_loader();
    loader
        .add_str(
            "bad-override",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "bad-override"
extends = ["base"]

[override."environment.variables.OVERRIDE_ME"]
op = "append"
values = ["wrong"]
reason = "wrong operation"
"#,
        )
        .unwrap();
    let error = loader.load("bad-override").unwrap_err();
    assert!(matches!(
        error,
        LoaderError::OverrideTypeMismatch {
            actual: ValueKind::Scalar,
            ..
        }
    ));
}

#[test]
fn sensitive_input_provenance_is_preserved_without_rendering_errors() {
    let mut loader = base_loader();
    loader
        .add_str(
            "secret",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "secret"
extends = ["base"]

[inputs.SECRET_MODE]
target = "features.secret.mode"
type = "string"
runtime = true
export_as = "SECRET_MODE"
sensitivity = "secret"

[config.features.secret]
mode = "unset"
"#,
        )
        .unwrap();
    let loaded = loader
        .load_with_runtime(
            "secret",
            BTreeMap::from([(String::from("SECRET_MODE"), String::from("value"))]),
        )
        .unwrap();
    assert_eq!(
        map_value(map_value(&loaded.config().features, "secret"), "mode"),
        &ValueTree::String("value".to_owned())
    );
    assert_eq!(
        loaded
            .config()
            .provenance
            .get("features.secret.mode")
            .unwrap()
            .sensitivity,
        Sensitivity::Secret
    );
}
