use crate::error::CoreError;
use crate::filesystem;
use crate::identity::ResolvedIdentity;
use bootstrap_model::Action;
use container_init_posix::{
    bind_mount, mount_cgroup2, unshare_user_and_mount_namespaces, ActionChange, PosixSystem,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) fn init_v2(
    action: &Action,
    identity: &ResolvedIdentity,
    values: &BTreeMap<String, String>,
    posix_system: &PosixSystem,
    preserve_pid: Option<u32>,
) -> Result<ActionChange, CoreError> {
    let action_id = action.id.as_str();
    let mount_mode = action.cgroup_mount_mode();
    let preserved_pid = preserve_pid.map(|pid| pid.to_string());

    let target_root = match &action.path {
        Some(path_template) => filesystem::render_path(action_id, "path", path_template, values)?,
        None => PathBuf::from("/sys/fs/cgroup"),
    };

    let (working_root, is_bind_mount) = if mount_mode == "bind_mount" {
        let shadow_root = match &action.shadow_path {
            Some(tpl) => filesystem::render_path(action_id, "shadow_path", tpl, values)?,
            None => PathBuf::from("/run/cgroup"),
        };
        // If the shadow path doesn't already have cgroup.controllers, attempt to mount cgroup2
        if !shadow_root.join("cgroup.controllers").exists() {
            if let Err(initial_mount_err) = mount_cgroup2(&shadow_root) {
                if preserve_pid.is_some() {
                    return Err(CoreError::action(
                        action_id,
                        Some(shadow_root),
                        format!(
                            "backend cannot perform cgroup mount fallback in its own namespace: {initial_mount_err}"
                        ),
                    ));
                }
                if !identity.run_as_root && identity.uid != 0 {
                    // In an unprivileged container where target is non-root, entering a single-user
                    // namespace would prevent subsequent drop_privileges to target UID.
                    if target_root.join("cgroup.controllers").exists() {
                        return Ok(ActionChange::new(format!(
                            "unprivileged non-root environment: preserving existing cgroup hierarchy at {} without shadow mount",
                            target_root.display()
                        )));
                    } else {
                        return Ok(ActionChange::new(
                            "unprivileged non-root environment: cgroup v2 hierarchy unavailable",
                        ));
                    }
                }
                // For root target, enter private user and mount namespace
                let (uid, gid) = posix_system.current_ids();
                unshare_user_and_mount_namespaces(uid, gid).map_err(|err| {
                    CoreError::action(
                        action_id,
                        Some(shadow_root.clone()),
                        format!(
                            "failed to mount cgroup2 directly ({initial_mount_err}) and failed to enter private namespace: {err}"
                        ),
                    )
                })?;
                mount_cgroup2(&shadow_root).map_err(|err| {
                    CoreError::action(
                        action_id,
                        Some(shadow_root.clone()),
                        format!("failed to mount cgroup2 in private namespace: {err}"),
                    )
                })?;
            }
        }
        (shadow_root, true)
    } else {
        (target_root.clone(), false)
    };

    if !working_root.exists() {
        return Err(CoreError::action(
            action_id,
            Some(working_root),
            "cgroup root path does not exist",
        ));
    }

    let controllers_file = working_root.join("cgroup.controllers");
    if !controllers_file.exists() {
        return Err(CoreError::action(
            action_id,
            Some(controllers_file),
            "path is not a cgroup v2 hierarchy (cgroup.controllers not found)",
        ));
    }

    let subgroup_name = action.subgroup.as_deref().unwrap_or("libpod_parent");
    let subgroup_path = working_root.join(subgroup_name);
    if !subgroup_path.exists() {
        fs::create_dir_all(&subgroup_path).map_err(|source| {
            CoreError::io(Some(action_id), Some(subgroup_path.clone()), source)
        })?;
    }

    // 1. Move processes from working_root/cgroup.procs to subgroup_path/cgroup.procs
    let root_procs_file = working_root.join("cgroup.procs");
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
                    if preserved_pid.as_deref() == Some(pid) {
                        continue;
                    }
                    // cgroup.procs parses one complete PID record per write.
                    // `writeln!` may issue separate writes for the PID and
                    // newline, making the second write fail with EINVAL on a
                    // real cgroupfs.
                    let write_res = append_pid(&subgroup_procs_file, pid);
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

    // `controllers` is the required set. Keep the old default (all visible
    // controllers required) when neither required nor optional sets are
    // configured. Once optional controllers are configured, an omitted
    // required set means that no controller is mandatory.
    let required_controllers: Vec<String> = match &action.controllers {
        Some(configured) => {
            for controller in configured {
                if !available.contains(controller.as_str()) {
                    return Err(CoreError::action(
                        action_id,
                        Some(controllers_file.clone()),
                        format!(
                            "required controller {controller:?} is not available in cgroup.controllers"
                        ),
                    ));
                }
            }
            configured.clone()
        }
        None if action.optional_controllers.is_some() => Vec::new(),
        None => available.iter().map(|s| s.to_string()).collect(),
    };

    let mut skipped_controllers = Vec::new();
    let mut target_controllers: Vec<(String, bool)> = required_controllers
        .into_iter()
        .map(|controller| (controller, true))
        .collect();
    if let Some(configured) = &action.optional_controllers {
        for controller in configured {
            if available.contains(controller.as_str()) {
                target_controllers.push((controller.clone(), false));
            } else {
                skipped_controllers.push(format!(
                    "{controller} (not available in cgroup.controllers)"
                ));
            }
        }
    }

    // 3. Read currently enabled controllers in cgroup.subtree_control
    let subtree_control_file = working_root.join("cgroup.subtree_control");
    let currently_enabled: BTreeSet<String> = if subtree_control_file.exists() {
        fs::read_to_string(&subtree_control_file)
            .map(|content| content.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    } else {
        BTreeSet::new()
    };

    // cgroup v2 refuses enabling a controller while the parent cgroup still
    // contains processes. The backend supervisor must remain outside the
    // delegated subgroup, so move it only for this short kernel operation and
    // restore it before any later action or handoff runs.
    let preserved_process = preserve_pid
        .filter(|pid| {
            !is_bind_mount
                && target_controllers
                    .iter()
                    .any(|(controller, _)| !currently_enabled.contains(controller))
                && fs::read_to_string(&root_procs_file)
                    .ok()
                    .is_some_and(|procs| procs.lines().any(|line| line.trim() == pid.to_string()))
        })
        .map(|pid| PreservedProcess::enter(&root_procs_file, &subgroup_procs_file, pid.to_string()))
        .transpose()
        .map_err(|source| {
            CoreError::io(Some(action_id), Some(subgroup_procs_file.clone()), source)
        })?;

    let mut enabled_controllers = Vec::new();
    for (controller, required) in &target_controllers {
        if !currently_enabled.contains(controller) {
            let cmd = format!("+{controller} ");
            let mut write_result = OpenOptions::new()
                .append(true)
                .open(&subtree_control_file)
                .and_then(|mut f| f.write_all(cmd.as_bytes()));

            // If EBUSY, drain any newly spawned processes in root cgroup.procs and retry
            if let Err(ref err) = write_result {
                if err.raw_os_error() == Some(16) {
                    if preserve_pid.is_some() && is_bind_mount {
                        // A freshly mounted shadow hierarchy may not contain
                        // the backend process, so moving it there would fail
                        // with EINVAL. Required controllers must fail closed;
                        // optional controllers can be recorded and skipped.
                        if *required {
                            return Err(CoreError::action(
                                action_id,
                                Some(subtree_control_file.clone()),
                                format!(
                                    "cannot enable required cgroup controller {controller:?} while preserving the backend supervisor in bind-mount mode"
                                ),
                            ));
                        }
                        skipped_controllers
                            .push(format!("{controller} (busy in bind-mount hierarchy)"));
                        continue;
                    }
                    if let Ok(procs_content) = fs::read_to_string(&root_procs_file) {
                        for line in procs_content.lines() {
                            let pid = line.trim();
                            if !pid.is_empty() && preserved_pid.as_deref() != Some(pid) {
                                let _ = append_pid(&subgroup_procs_file, pid);
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
                if *required {
                    return Err(CoreError::io(
                        Some(action_id),
                        Some(subtree_control_file.clone()),
                        source,
                    ));
                }
                skipped_controllers.push(format!("{controller} ({source})"));
                continue;
            }
            enabled_controllers.push(controller.clone());
        }
    }
    drop(preserved_process);

    if action.subgroup_type.is_some()
        || action.subgroup_controllers.is_some()
        || action.subgroup_controller_values.is_some()
    {
        initialize_subgroup(action_id, &working_root, &subgroup_path, action)?;
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

    // 5. If mount_mode is bind_mount, bind-mount working_root over target_root
    if is_bind_mount {
        bind_mount(&working_root, &target_root).map_err(|source| {
            CoreError::action(
                action_id,
                Some(target_root.clone()),
                format!("failed to bind-mount {working_root:?} over {target_root:?}: {source}"),
            )
        })?;
    }

    let target_names = target_controllers
        .iter()
        .map(|(controller, _)| controller.as_str())
        .collect::<Vec<_>>();
    let mut message = if !enabled_controllers.is_empty() {
        if is_bind_mount {
            format!(
                "delegated cgroup v2 controllers ({}) to subtree_control; moved {moved_pids} procs to {subgroup_name}; shadowed to {}",
                enabled_controllers.join(" "),
                target_root.display()
            )
        } else {
            format!(
                "delegated cgroup v2 controllers ({}) to subtree_control; moved {moved_pids} procs to {subgroup_name}",
                enabled_controllers.join(" ")
            )
        }
    } else {
        if is_bind_mount {
            format!(
                "cgroup v2 controllers ({}) already enabled in subtree_control; {moved_pids} procs in {subgroup_name}; shadowed to {}",
                target_names.join(" "),
                target_root.display()
            )
        } else {
            format!(
                "cgroup v2 controllers ({}) already enabled in subtree_control; {moved_pids} procs in {subgroup_name}",
                target_names.join(" ")
            )
        }
    };

    if !skipped_controllers.is_empty() {
        message.push_str(&format!(
            "; skipped optional controllers: {}",
            skipped_controllers.join(", ")
        ));
    }

    Ok(ActionChange::new(message))
}

fn initialize_subgroup(
    action_id: &str,
    working_root: &Path,
    subgroup_path: &Path,
    action: &Action,
) -> Result<(), CoreError> {
    let subgroup_controllers = subgroup_path.join("cgroup.controllers");
    let subgroup_subtree = subgroup_path.join("cgroup.subtree_control");
    if !subgroup_controllers.exists() || !subgroup_subtree.exists() {
        return Err(CoreError::action(
            action_id,
            Some(subgroup_path.to_path_buf()),
            "configured subgroup is missing cgroup control files",
        ));
    }

    if let Some(subgroup_type) = &action.subgroup_type {
        write_cgroup_value(action_id, &subgroup_path.join("cgroup.type"), subgroup_type)?;
    }

    let available: BTreeSet<String> = fs::read_to_string(&subgroup_controllers)
        .map_err(|source| {
            CoreError::io(Some(action_id), Some(subgroup_controllers.clone()), source)
        })?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let delegated: BTreeSet<String> =
        fs::read_to_string(working_root.join("cgroup.subtree_control"))
            .map_err(|source| {
                CoreError::io(
                    Some(action_id),
                    Some(working_root.join("cgroup.subtree_control")),
                    source,
                )
            })?
            .split_whitespace()
            .map(|controller| controller.trim_start_matches('+').to_owned())
            .collect();
    if let Some(values) = &action.subgroup_controller_values {
        for (target_name, source_name) in values {
            let source = working_root.join(source_name);
            let target = subgroup_path.join(target_name);
            let value = fs::read_to_string(&source).map_err(|source_error| {
                CoreError::io(Some(action_id), Some(source), source_error)
            })?;
            write_cgroup_value(action_id, &target, value.trim())?;
        }
    }

    let enabled: BTreeSet<String> = fs::read_to_string(&subgroup_subtree)
        .map_err(|source| CoreError::io(Some(action_id), Some(subgroup_subtree.clone()), source))?
        .split_whitespace()
        .map(|controller| controller.trim_start_matches('+').to_owned())
        .collect();

    for controller in action.subgroup_controllers.iter().flatten() {
        if !delegated.contains(controller) || !available.contains(controller) {
            return Err(CoreError::action(
                action_id,
                Some(subgroup_controllers.clone()),
                format!(
                    "configured subgroup controller {controller:?} is not delegated or available"
                ),
            ));
        }
        if !enabled.contains(controller) {
            append_cgroup_value(action_id, &subgroup_subtree, &format!("+{controller}"))?;
        }
    }

    Ok(())
}

fn write_cgroup_value(action_id: &str, path: &Path, value: &str) -> Result<(), CoreError> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{value}\n").as_bytes()))
        .map_err(|source| CoreError::io(Some(action_id), Some(path.to_path_buf()), source))
}

fn append_cgroup_value(action_id: &str, path: &Path, value: &str) -> Result<(), CoreError> {
    OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{value}\n").as_bytes()))
        .map_err(|source| CoreError::io(Some(action_id), Some(path.to_path_buf()), source))
}

struct PreservedProcess {
    root_procs: PathBuf,
    pid: String,
}

impl PreservedProcess {
    fn enter(root_procs: &Path, subgroup_procs: &Path, pid: String) -> std::io::Result<Self> {
        append_pid(subgroup_procs, &pid)?;
        Ok(Self {
            root_procs: root_procs.to_path_buf(),
            pid,
        })
    }
}

impl Drop for PreservedProcess {
    fn drop(&mut self) {
        let _ = append_pid(&self.root_procs, &self.pid);
    }
}

fn append_pid(path: &Path, pid: &str) -> std::io::Result<()> {
    let record = format!("{pid}\n");
    OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(record.as_bytes()))
}
