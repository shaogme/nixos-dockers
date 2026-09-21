use crate::error::CoreError;
use crate::filesystem;
use crate::identity::ResolvedIdentity;
use bootstrap_model::Action;
use container_init_posix::{ActionChange, PosixSystem};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub(crate) fn init_v2(
    action: &Action,
    identity: &ResolvedIdentity,
    values: &BTreeMap<String, String>,
    posix_system: &PosixSystem,
) -> Result<ActionChange, CoreError> {
    let action_id = action.id.as_str();
    let cgroup_root = match &action.path {
        Some(path_template) => filesystem::render_path(action_id, "path", path_template, values)?,
        None => PathBuf::from("/sys/fs/cgroup"),
    };

    if !cgroup_root.exists() {
        return Err(CoreError::action(
            action_id,
            Some(cgroup_root),
            "cgroup root path does not exist",
        ));
    }

    let controllers_file = cgroup_root.join("cgroup.controllers");
    if !controllers_file.exists() {
        return Err(CoreError::action(
            action_id,
            Some(controllers_file),
            "path is not a cgroup v2 hierarchy (cgroup.controllers not found)",
        ));
    }

    let subgroup_name = action.subgroup.as_deref().unwrap_or("init");
    let subgroup_path = cgroup_root.join(subgroup_name);
    if !subgroup_path.exists() {
        fs::create_dir_all(&subgroup_path).map_err(|source| {
            CoreError::io(Some(action_id), Some(subgroup_path.clone()), source)
        })?;
    }

    // 1. Move processes from cgroup_root/cgroup.procs to subgroup_path/cgroup.procs
    let root_procs_file = cgroup_root.join("cgroup.procs");
    let subgroup_procs_file = subgroup_path.join("cgroup.procs");
    if !subgroup_procs_file.exists() {
        // In real cgroupfs, cgroup.procs is automatically created by the kernel upon mkdir.
        // In mock filesystems (such as unit tests), ensure the file exists.
        let _ = fs::File::create(&subgroup_procs_file);
    }
    let mut moved_pids = 0usize;
    if root_procs_file.exists() && subgroup_procs_file.exists() {
        for _ in 0..5 {
            let mut drained_in_pass = 0usize;
            if let Ok(procs_content) = fs::read_to_string(&root_procs_file) {
                for line in procs_content.lines() {
                    let pid = line.trim();
                    if pid.is_empty() {
                        continue;
                    }
                    let write_res = OpenOptions::new()
                        .append(true)
                        .open(&subgroup_procs_file)
                        .and_then(|mut f| writeln!(f, "{pid}"));
                    if write_res.is_ok() {
                        drained_in_pass += 1;
                    }
                }
            }
            moved_pids += drained_in_pass;
            if drained_in_pass == 0 {
                break;
            }
        }
    }

    // 2. Read available controllers from cgroup.controllers
    let raw_controllers = fs::read_to_string(&controllers_file)
        .map_err(|source| CoreError::io(Some(action_id), Some(controllers_file.clone()), source))?;
    let available: BTreeSet<&str> = raw_controllers.split_whitespace().collect();

    let target_controllers: Vec<String> = if let Some(configured) = &action.controllers {
        for controller in configured {
            if !available.contains(controller.as_str()) {
                return Err(CoreError::action(
                    action_id,
                    Some(controllers_file),
                    format!("requested controller {controller:?} is not available in cgroup.controllers"),
                ));
            }
        }
        configured.clone()
    } else {
        available.iter().map(|s| s.to_string()).collect()
    };

    // 3. Read currently enabled controllers in cgroup.subtree_control
    let subtree_control_file = cgroup_root.join("cgroup.subtree_control");
    let currently_enabled: BTreeSet<String> = if subtree_control_file.exists() {
        fs::read_to_string(&subtree_control_file)
            .map(|content| content.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    } else {
        BTreeSet::new()
    };

    let mut enabled_controllers = Vec::new();
    for controller in &target_controllers {
        if !currently_enabled.contains(controller) {
            let cmd = format!("+{controller} ");
            let mut write_result = OpenOptions::new()
                .append(true)
                .open(&subtree_control_file)
                .and_then(|mut f| f.write_all(cmd.as_bytes()));

            // If EBUSY, drain any newly spawned processes in root cgroup.procs and retry
            if let Err(ref err) = write_result {
                if err.kind() == std::io::ErrorKind::ResourceBusy || err.raw_os_error() == Some(16)
                {
                    if let Ok(procs_content) = fs::read_to_string(&root_procs_file) {
                        for line in procs_content.lines() {
                            let pid = line.trim();
                            if !pid.is_empty() {
                                let _ = OpenOptions::new()
                                    .append(true)
                                    .open(&subgroup_procs_file)
                                    .and_then(|mut f| writeln!(f, "{pid}"));
                            }
                        }
                    }
                    write_result = OpenOptions::new()
                        .append(true)
                        .open(&subtree_control_file)
                        .and_then(|mut f| f.write_all(cmd.as_bytes()));
                }
            }

            if let Err(source) = write_result {
                return Err(CoreError::io(
                    Some(action_id),
                    Some(subtree_control_file),
                    source,
                ));
            }
            enabled_controllers.push(controller.clone());
        }
    }

    // 4. If an owner is specified, reconcile ownership of the subgroup directory
    if let Some(owner) = &action.owner {
        filesystem::chown(
            action_id,
            &subgroup_path,
            owner,
            false,
            identity,
            posix_system,
        )?;
    }

    let message = if !enabled_controllers.is_empty() {
        format!(
            "delegated cgroup v2 controllers ({}) to subtree_control; moved {moved_pids} procs to {subgroup_name}",
            enabled_controllers.join(" ")
        )
    } else {
        format!(
            "cgroup v2 controllers ({}) already enabled in subtree_control; {moved_pids} procs in {subgroup_name}",
            target_controllers.join(" ")
        )
    };

    Ok(ActionChange::new(message))
}
