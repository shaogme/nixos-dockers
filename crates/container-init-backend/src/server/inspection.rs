use super::runtime::{BackendRuntime, RuntimeErrors};
use container_init_protocol::{
    BackendError, PlanPage, ServerMessage, ServerResponse, MAX_PLAN_PAGE_ACTIONS,
    MAX_RESPONSE_FRAME_BYTES, PROTOCOL_VERSION,
};
use serde_json::{json, to_value, to_vec, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

pub(super) struct BackendInspection;

impl BackendInspection {
    pub(super) fn plan_page(
        runtime: &BackendRuntime,
        requested_snapshot: Option<&str>,
        offset: usize,
    ) -> Result<ServerResponse, BackendError> {
        plan_page_response(runtime, requested_snapshot, offset)
    }

    pub(super) fn doctor(runtime: &BackendRuntime) -> Value {
        doctor_response(runtime)
    }
}

fn plan_page_response(
    runtime: &BackendRuntime,
    requested_snapshot: Option<&str>,
    offset: usize,
) -> Result<ServerResponse, BackendError> {
    let actions = runtime.plan().actions();
    let total_actions = actions.len();
    if offset > total_actions
        || (offset == 0
            && requested_snapshot.is_some_and(|snapshot| snapshot != runtime.snapshot_id()))
        || (offset != 0 && requested_snapshot != Some(runtime.snapshot_id()))
    {
        return Err(RuntimeErrors::backend(
            "invalid_cursor",
            false,
            "plan cursor does not match the current backend snapshot",
        ));
    }
    let remaining = total_actions - offset;
    let max_count = remaining.min(MAX_PLAN_PAGE_ACTIONS);
    if max_count == 0 {
        return Ok(ServerResponse::PlanPage(PlanPage {
            online: true,
            profile: runtime.profile().to_owned(),
            snapshot_id: runtime.snapshot_id().to_owned(),
            offset,
            next_offset: None,
            total_actions,
            actions: Vec::new(),
        }));
    }
    for count in (1..=max_count).rev() {
        let page_actions = actions[offset..offset + count]
            .iter()
            .map(to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                RuntimeErrors::backend("internal", false, "could not serialize plan page")
            })?;
        let next_offset = (offset + count < total_actions).then_some(offset + count);
        let page = PlanPage {
            online: true,
            profile: runtime.profile().to_owned(),
            snapshot_id: runtime.snapshot_id().to_owned(),
            offset,
            next_offset,
            total_actions,
            actions: page_actions,
        };
        let response = ServerResponse::PlanPage(page);
        let envelope = ServerMessage {
            version: PROTOCOL_VERSION,
            response: response.clone(),
        };
        let payload = to_vec(&envelope).map_err(|_| {
            RuntimeErrors::backend("internal", false, "could not serialize plan page")
        })?;
        if payload.len() <= MAX_RESPONSE_FRAME_BYTES {
            return Ok(response);
        }
    }
    let action = &actions[offset];
    let action_bytes = to_vec(action).map_or(0, |bytes| bytes.len());
    Err(RuntimeErrors::backend(
        "response_too_large",
        false,
        &format!(
            "plan action {:?} serializes to {action_bytes} bytes and exceeds the {MAX_RESPONSE_FRAME_BYTES} byte response frame limit",
            action.id
        ),
    ))
}

fn doctor_response(runtime: &BackendRuntime) -> Value {
    let executable = Path::new(&runtime.config().handoff.runtime);
    let exists = executable.is_file();
    let executable_ok = is_executable(executable);
    json!({
        "online": true,
        "ok": exists && executable_ok,
        "profile": runtime.profile(),
        "snapshot_id": runtime.snapshot_id(),
        "backend": runtime.status(),
        "identity": runtime.startup_identity(),
        "handoff": {
            "runtime": runtime.config().handoff.runtime,
            "exists": exists,
            "executable": executable_ok,
        },
    })
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
