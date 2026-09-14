use crate::config::LoadedConfig;
use crate::doctor::DoctorReport;
use crate::error::CliError;
use bootstrap_model::Plan;
use serde::Serialize;

#[derive(Serialize)]
struct JsonPlan<'a> {
    profile: &'a str,
    profile_chain: Vec<&'a str>,
    #[serde(flatten)]
    plan: &'a Plan,
}

pub fn print_plan(loaded: &LoadedConfig, json: bool) -> Result<(), CliError> {
    if json {
        let value = JsonPlan {
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
        let rendered = serde_json::to_string_pretty(report).map_err(CliError::Output)?;
        println!("{rendered}");
        return Ok(());
    }

    println!("profile: {}", report.profile);
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
