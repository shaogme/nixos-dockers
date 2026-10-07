use crate::config::LoadedConfig;
use crate::error::{CliError, OutputError};
use dev_env_model::{MergePolicy, Origin, Sensitivity};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct ExplainReport {
    pub profile: String,
    pub profile_chain: Vec<String>,
    pub workspace: String,
    pub cwd: String,
    pub merge_policy: MergePolicy,
    pub config_fingerprint: String,
    pub path: Option<String>,
    pub value: Option<Value>,
    pub provenance: BTreeMap<String, ExplainProvenance>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExplainProvenance {
    pub sensitivity: Sensitivity,
    pub origins: Vec<Origin>,
}

pub fn build_report(loaded: &LoadedConfig, path: Option<&str>) -> Result<ExplainReport, CliError> {
    let encoded = serde_json::to_vec(loaded.config()).map_err(OutputError::Json)?;
    let config = serde_json::to_value(loaded.config()).map_err(OutputError::Json)?;
    let value = path
        .map(|path| {
            let value = value_at(&config, path).ok_or_else(|| {
                CliError::Configuration(crate::error::ConfigurationError::MissingConfigPath {
                    path: path.to_owned(),
                })
            })?;
            let sensitivity = loaded
                .config()
                .provenance
                .get(path)
                .map(|entry| entry.sensitivity)
                .unwrap_or_default();
            Ok::<Value, CliError>(if sensitivity == Sensitivity::Public {
                value.clone()
            } else {
                Value::String("<redacted>".to_owned())
            })
        })
        .transpose()?;
    let provenance = loaded
        .config()
        .provenance
        .iter()
        .map(|(path, entry)| {
            (
                path.to_owned(),
                ExplainProvenance {
                    sensitivity: entry.sensitivity,
                    origins: entry.origins.clone(),
                },
            )
        })
        .collect();
    Ok(ExplainReport {
        profile: loaded.profile().to_owned(),
        profile_chain: loaded
            .profile_chain()
            .iter()
            .map(|profile| profile.id.clone())
            .collect(),
        workspace: loaded.workspace().display().to_string(),
        cwd: loaded.cwd().display().to_string(),
        merge_policy: loaded.config().policy.merge,
        config_fingerprint: hex_digest(dev_env_provider::fingerprint_bytes(&encoded)),
        path: path.map(str::to_owned),
        value,
        provenance,
    })
}

pub fn print_report(report: &ExplainReport, json: bool) -> Result<(), CliError> {
    if json {
        let rendered = serde_json::to_string_pretty(report).map_err(OutputError::Json)?;
        crate::output::write_stdout(&format!("{rendered}\n"))
    } else {
        let mut rendered = String::new();
        rendered.push_str(&format!("profile: {}\n", report.profile));
        rendered.push_str(&format!(
            "profile-chain: {}\n",
            report.profile_chain.join(" -> ")
        ));
        rendered.push_str(&format!("workspace: {}\n", report.workspace));
        rendered.push_str(&format!("cwd: {}\n", report.cwd));
        rendered.push_str(&format!("merge-policy: {:?}\n", report.merge_policy));
        rendered.push_str(&format!(
            "config-fingerprint: {}\n",
            report.config_fingerprint
        ));
        if let Some(path) = &report.path {
            rendered.push_str(&format!("{path}: "));
            if let Some(value) = &report.value {
                rendered.push_str(&serde_json::to_string(value).map_err(OutputError::Json)?);
            } else {
                rendered.push_str("<missing>");
            }
            rendered.push('\n');
        }
        rendered.push_str("provenance:\n");
        for (path, entry) in &report.provenance {
            rendered.push_str(&format!("  {path} ({:?}):\n", entry.sensitivity));
            for origin in &entry.origins {
                rendered.push_str(&format!("    - {origin:?}\n"));
            }
        }
        crate::output::write_stdout(&rendered)
    }
}

fn value_at<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut value = root;
    for component in path.split('.') {
        value = value.as_object()?.get(component)?;
    }
    Some(value)
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
