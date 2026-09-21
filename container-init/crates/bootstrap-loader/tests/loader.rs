use bootstrap_loader::{LoaderError, ProfileLoader, ProfileSource};
use bootstrap_model::{ActionKind, ModelError, SourceKind};
use std::error::Error;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const BASE: &str = r#"
schema = 1
id = "base"

[config.providers.not-a-bootstrap-provider]
executable = "must-not-be-read-by-this-loader"

[bootstrap]
workspace_root = "/srv/workspace"
allow_workspace_overlay = true

[bootstrap.identity]
default_user = "builder"
default_uid = 1000
default_gid = 1000
auto_mapping = true

[bootstrap.handoff]
runtime = "/usr/bin/dev-env"
exec_prefix = ["exec", "--"]
shell_prefix = ["shell"]

[bootstrap.policy]
workspace_safe_action_kinds = ["filesystem.ensure_dir", "filesystem.ensure_symlink"]

[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "workspace"
kind = "filesystem.ensure_dir"
path = "${bootstrap.workspace_root}"
mode = "0777"
owner = "root"
run_as = "root"
"#;

fn base_loader() -> ProfileLoader {
    let mut loader = ProfileLoader::new();
    loader
        .add_profile(ProfileSource::with_location(
            "base",
            SourceKind::ImageProfile,
            "/profiles/00-base.toml",
            BASE,
        ))
        .expect("base profile should be accepted");
    loader
}

#[test]
fn projects_only_bootstrap_and_keeps_parent_to_child_provenance() {
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
ONLY_ENVIRONMENT = "ignored"

[[bootstrap.actions]]
id = "state"
kind = "filesystem.ensure_dir"
path = "/var/lib/state"
mode = "0755"
owner = "root"
run_as = "root"
"#,
        )
        .expect("derived profile should be accepted");

    let loaded = loader.load("derived").expect("profile graph should load");
    assert_eq!(loaded.config().workspace_root, "/srv/workspace");
    assert!(!loaded.config().actions.iter().any(|action| {
        action
            .path
            .as_deref()
            .is_some_and(|path| path.contains("ONLY_ENVIRONMENT"))
    }));
    assert_eq!(loaded.profile_chain().len(), 2);
    assert_eq!(loaded.profile_chain()[0].id, "base");
    assert_eq!(loaded.profile_chain()[1].id, "derived");
    assert_eq!(loaded.config().actions[2].origin.profile, "derived");
    assert_eq!(
        loaded.config().actions[2].origin.source,
        SourceKind::ImageProfile
    );
    assert!(loaded.config().actions[2]
        .origin
        .location
        .as_deref()
        .is_some_and(|location| location.contains("bootstrap.actions[0]")));

    let plan = loaded
        .build_plan()
        .expect("loaded config should build a plan");
    assert_eq!(plan.actions()[2].kind, ActionKind::FilesystemEnsureDir);
}

#[test]
fn action_conflicts_require_a_reasoned_explicit_override() {
    let mut loader = base_loader();
    loader
        .add_str(
            "child",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "child"
extends = ["base"]

[[bootstrap.actions]]
id = "workspace"
kind = "filesystem.ensure_dir"
path = "/srv/other"
mode = "0777"
owner = "root"
run_as = "root"
"#,
        )
        .unwrap();

    let error = loader
        .load("child")
        .expect_err("replacement must be explicit");
    let rendered = error.to_string();
    assert!(rendered.contains("bootstrap.actions.workspace"));
    assert!(rendered.contains("base"));
    assert!(rendered.contains("child"));
    assert!(rendered.contains("override"));

    let mut loader = base_loader();
    loader
        .add_str(
            "child",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "child"
extends = ["base"]

[override."bootstrap.actions.workspace"]
op = "set"
reason = "move the workspace mount for this image"

[[bootstrap.actions]]
id = "workspace"
kind = "filesystem.ensure_dir"
path = "/srv/other"
mode = "0777"
owner = "root"
run_as = "root"
"#,
        )
        .unwrap();

    let loaded = loader.load("child").expect("reasoned override should load");
    assert_eq!(
        loaded.config().actions[1].path.as_deref(),
        Some("/srv/other")
    );
    assert_eq!(
        loaded.config().actions[1].origin.location.as_deref(),
        Some("child:bootstrap.actions[0]")
    );
}

#[test]
fn scalar_conflicts_also_require_explicit_overrides() {
    let mut loader = base_loader();
    loader
        .add_str(
            "child",
            SourceKind::AdminProfile,
            r#"
schema = 1
id = "child"
extends = ["base"]

[bootstrap]
workspace_root = "/admin/workspace"
"#,
        )
        .unwrap();
    assert!(matches!(
        loader.load("child"),
        Err(LoaderError::Conflict { path, .. }) if path == "bootstrap.workspace_root"
    ));

    let mut loader = base_loader();
    loader
        .add_str(
            "child",
            SourceKind::AdminProfile,
            r#"
schema = 1
id = "child"
extends = ["base"]

[override."bootstrap.workspace_root"]
reason = "the administrator mounted workspaces elsewhere"

[bootstrap]
workspace_root = "/admin/workspace"
"#,
        )
        .unwrap();
    assert_eq!(
        loader
            .load("child")
            .expect("explicit scalar override should load")
            .config()
            .workspace_root,
        "/admin/workspace"
    );
}

#[test]
fn workspace_profiles_can_only_add_safe_target_actions() {
    let mut loader = base_loader();
    loader
        .add_str(
            "workspace-overlay",
            SourceKind::WorkspaceOverlay,
            r#"
schema = 1
id = "workspace-overlay"
extends = ["base"]

[[bootstrap.actions]]
id = "cache"
kind = "filesystem.ensure_dir"
path = "${bootstrap.workspace_root}/.cache"
owner = "identity.target"
run_as = "target"
"#,
        )
        .unwrap();

    let loaded = loader
        .load("workspace-overlay")
        .expect("safe workspace action should load");
    assert_eq!(
        loaded.config().actions[2].origin.source,
        SourceKind::WorkspaceOverlay
    );
    assert!(loaded.build_plan().is_ok());

    let mut loader = base_loader();
    loader
        .add_str(
            "unsafe-workspace",
            SourceKind::WorkspaceOverlay,
            r#"
schema = 1
id = "unsafe-workspace"
extends = ["base"]

[bootstrap.identity]
default_uid = 0
"#,
        )
        .unwrap();
    let error = loader
        .load("unsafe-workspace")
        .expect_err("workspace must not modify identity");
    assert!(matches!(error, LoaderError::TrustViolation { .. }));
}

#[test]
fn untrusted_action_replacement_is_rejected_even_with_an_override_declaration() {
    let mut loader = base_loader();
    loader
        .add_str(
            "workspace-overlay",
            SourceKind::WorkspaceOverlay,
            r#"
schema = 1
id = "workspace-overlay"
extends = ["base"]

[[bootstrap.actions]]
id = "workspace"
kind = "filesystem.ensure_dir"
path = "/tmp/replaced"
owner = "identity.target"
run_as = "target"

[override."bootstrap.actions.workspace"]
reason = "must not grant a workspace replacement"
"#,
        )
        .unwrap();
    let error = loader
        .load("workspace-overlay")
        .expect_err("workspace cannot replace inherited actions");
    assert!(matches!(error, LoaderError::TrustViolation { .. }));
}

#[test]
fn graph_errors_and_bootstrap_schema_errors_are_deterministic() {
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
    assert!(matches!(
        loader.load("a"),
        Err(LoaderError::InheritanceCycle(cycle))
            if cycle == vec!["a", "b", "a"]
    ));
    assert!(matches!(
        loader.load("missing"),
        Err(LoaderError::MissingProfile(id)) if id == "missing"
    ));

    let mut loader = ProfileLoader::new();
    let error = loader
        .add_str(
            "invalid",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "invalid"

[bootstrap]
workspace_root = "/safe"
unsupported = true
"#,
        )
        .expect_err("unknown bootstrap fields must be rejected while loading the source");
    assert!(matches!(&error, LoaderError::Parse { .. }));
    assert!(error.source().is_some());
}

#[test]
fn model_validation_still_rejects_shell_fragments_in_projected_actions() {
    let mut loader = ProfileLoader::new();
    loader
        .add_str(
            "unsafe",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "unsafe"

[bootstrap]
workspace_root = "/safe"

[bootstrap.handoff]
runtime = "/usr/bin/runtime"

[[bootstrap.actions]]
id = "bad"
kind = "filesystem.ensure_dir"
path = "$(touch /tmp/pwned)"
"#,
        )
        .unwrap();
    let error = loader
        .load("unsafe")
        .expect_err("shell syntax must be rejected");
    assert!(matches!(
        error,
        LoaderError::Model(ModelError::Invalid { message, .. }) if message.contains("shell expressions")
    ));
}

#[test]
fn directory_loader_indexes_declared_ids_instead_of_filenames() {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after the epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "bootstrap-loader-test-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir(&directory).expect("test directory should be created");
    fs::write(directory.join("arbitrary-name.toml"), BASE).expect("profile should be written");
    fs::write(directory.join("ignored.txt"), "not toml").expect("non-profile should be written");

    let loader = ProfileLoader::from_directory(&directory).expect("directory should load");
    assert_eq!(loader.profile_ids().collect::<Vec<_>>(), vec!["base"]);
    assert!(loader.load("base").is_ok());

    fs::remove_dir_all(&directory).expect("test directory should be removed");
}

#[test]
fn user_overlays_cannot_enter_the_bootstrap_projection() {
    let mut loader = base_loader();
    loader
        .add_str(
            "user-overlay",
            SourceKind::UserOverlay,
            r#"
schema = 1
id = "user-overlay"
extends = ["base"]

[[bootstrap.actions]]
id = "user-action"
kind = "filesystem.ensure_dir"
path = "/tmp/user-action"
run_as = "target"
"#,
        )
        .unwrap();
    let error = loader
        .load("user-overlay")
        .expect_err("user bootstrap overlays are not trusted");
    assert!(matches!(error, LoaderError::TrustViolation { .. }));
}

#[test]
fn profile_loader_loads_cgroup_v2_init() {
    let mut loader = base_loader();
    loader
        .add_str(
            "cgroup-profile",
            SourceKind::ImageProfile,
            r#"
schema = 1
id = "cgroup-profile"
extends = ["base"]

[[bootstrap.actions]]
id = "cgroup-init"
kind = "cgroup.v2_init"
subgroup = "init"
controllers = ["cpu", "memory"]
run_as = "root"
"#,
        )
        .unwrap();
    let loaded = loader.load("cgroup-profile").unwrap();
    let cg_action = loaded
        .config()
        .actions
        .iter()
        .find(|a| a.id == "cgroup-init")
        .expect("cgroup-init action should be present");
    assert_eq!(cg_action.kind, ActionKind::CgroupV2Init);
    assert_eq!(cg_action.subgroup.as_deref(), Some("init"));
    assert_eq!(
        cg_action.controllers.as_deref(),
        Some(&["cpu".to_string(), "memory".to_string()][..])
    );
}
