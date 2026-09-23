use crate::config::LoadedConfig;
use crate::doctor::DoctorReport;
use crate::error::CliError;
use bootstrap_model::Plan;
use serde::Serialize;

#[derive(Serialize)]
struct JsonPlan<'a> {
    online: bool,
    profile: &'a str,
    profile_chain: Vec<&'a str>,
    #[serde(flatten)]
    plan: &'a Plan,
}

pub fn print_plan(loaded: &LoadedConfig, json: bool) -> Result<(), CliError> {
    if json {
        let value = JsonPlan {
            online: false,
            profile: loaded.profile(),
            profile_chain: loaded
                .profile_chain()
                .map(|profile| profile.id.as_str())
                .collect(),
            plan: loaded.plan(),
        };
        let rendered = serde_json::to_string_pretty(&value).map_err(CliError::Output)?;
        println!("{rendered}");
        return Ok(());
    }

    println!("profile: {}", loaded.profile());
    println!("mode: offline");
    println!(
        "profile-chain: {}",
        loaded
            .profile_chain()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>()
            .join(" -> ")
    );
    for (index, action) in loaded.plan().actions().iter().enumerate() {
        println!(
            "{:>3}. {} kind={:?} phase={:?} run_as={:?} idempotency={:?} origin={:?} effect={:?}",
            index + 1,
            action.id,
            action.kind,
            action.phase,
            action.run_as,
            action.idempotency,
            action.origin,
            action.effect,
        );
    }
    Ok(())
}

pub fn print_doctor(report: &DoctorReport, json: bool) -> Result<(), CliError> {
    if json {
        let mut value = serde_json::to_value(report).map_err(CliError::Output)?;
        value["online"] = serde_json::Value::Bool(false);
        let rendered = serde_json::to_string_pretty(&value).map_err(CliError::Output)?;
        println!("{rendered}");
        return Ok(());
    }

    println!("profile: {}", report.profile);
    println!("mode: offline");
    println!("profile-chain: {}", report.profile_chain.join(" -> "));
    println!("workspace: {}", report.workspace);
    for check in &report.checks {
        print!(
            "{:>5} {}: {}",
            format!("{:?}", check.status).to_lowercase(),
            check.name,
            check.message
        );
        if let Some(error) = &check.error {
            print!(" ({error})");
        }
        println!();
    }
    println!("status: {}", if report.ok { "ok" } else { "failed" });
    Ok(())
}

pub fn print_backend(value: &serde_json::Value, json: bool, view: &str) -> Result<(), CliError> {
    if json {
        let rendered = serde_json::to_string_pretty(value).map_err(CliError::Output)?;
        println!("{rendered}");
        return Ok(());
    }

    println!("mode: online");
    for name in ["profile", "snapshot_id"] {
        if let Some(value) = value.get(name).and_then(serde_json::Value::as_str) {
            println!(
                "{}: {}",
                if name == "snapshot_id" {
                    "snapshot"
                } else {
                    name
                },
                value
            );
        }
    }
    match view {
        "plan" => {
            if let Some(actions) = value.get("actions").and_then(serde_json::Value::as_array) {
                for (index, action) in actions.iter().enumerate() {
                    println!(
                        "{:>3}. {} kind={} phase={}",
                        index + 1,
                        action
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("?"),
                        action.get("kind").unwrap_or(&serde_json::Value::Null),
                        action.get("phase").unwrap_or(&serde_json::Value::Null),
                    );
                }
            }
        }
        "doctor" => {
            if let Some(backend) = value.get("backend") {
                for name in ["state", "backend_pid", "active_requests"] {
                    if let Some(field) = backend.get(name) {
                        println!("{name}: {field}");
                    }
                }
            }
            if let Some(identity) = value.get("identity") {
                for name in ["uid", "gid", "user", "home"] {
                    if let Some(field) = identity.get(name) {
                        println!("identity_{name}: {field}");
                    }
                }
            }
            if let Some(handoff) = value.get("handoff") {
                if let Some(runtime) = handoff.get("runtime") {
                    println!("handoff_runtime: {runtime}");
                }
                if let Some(executable) = handoff.get("executable") {
                    println!("handoff_executable: {executable}");
                }
            }
        }
        "status" => {
            for name in [
                "state",
                "backend_pid",
                "initial_child_pid",
                "started_unix_seconds",
                "active_requests",
            ] {
                if let Some(field) = value.get(name) {
                    println!("{name}: {field}");
                }
            }
        }
        _ => {}
    }
    Ok(())
}
