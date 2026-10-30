use dev_env_model::{
    BackendError, BackendRequest, BackendRequestMessage, BackendResponse, BackendResponseMessage,
    BackendStatus, ControlPlaneState, EffectiveIdentity, Generation, IdentityRequest,
    InitialRuntimeState, RequestContext, RequestMode, DEVENV_BACKEND_ERROR_VERSION,
    DEVENV_BACKEND_PROTOCOL_VERSION,
};
use serde_json::to_value;
use std::collections::BTreeMap;

#[test]
fn backend_v5_request_round_trips_and_rejects_invalid_context() {
    let context = RequestContext::new("request-1", RequestMode::Exec, "/workspace", "bash")
        .with_identity(IdentityRequest::Peer)
        .with_runtime_input("DEVBOX_AUTO_INIT", "never")
        .with_ambient_environment(BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]));
    let original = BackendRequestMessage::new("request-1", BackendRequest::Prepare { context });
    let encoded = original.encode().unwrap();
    let decoded = BackendRequestMessage::decode(&encoded).unwrap();
    assert_eq!(decoded, original);
    assert_eq!(decoded.protocol(), DEVENV_BACKEND_PROTOCOL_VERSION);
    assert_eq!(DEVENV_BACKEND_PROTOCOL_VERSION, 5);

    let invalid = BackendRequestMessage::new(
        "request-2",
        BackendRequest::Prepare {
            context: RequestContext::new("request-2", RequestMode::Exec, "relative", "bash"),
        },
    );
    assert!(invalid.encode().is_err());

    let mut frame = Vec::new();
    original.write_to(&mut frame).unwrap();
    assert_eq!(
        BackendRequestMessage::read_from(&mut std::io::Cursor::new(frame)).unwrap(),
        original
    );
}

#[test]
fn backend_status_exposes_control_plane_and_initial_runtime_independently() {
    let status = BackendStatus {
        control_plane: ControlPlaneState::Serving,
        initial_runtime: InitialRuntimeState::Preparing,
        startup_phase: Some("initial_runtime_preparing".to_owned()),
        initial_runtime_exit_code: None,
        initial_runtime_error: None,
        generation: Generation::INITIAL,
        config_fingerprint: [0; 32],
        backend_pid: 42,
        active_requests: 0,
    };
    let value = to_value(status).unwrap();
    assert_eq!(value["control_plane"], "serving");
    assert_eq!(value["initial_runtime"], "preparing");
    assert_eq!(value["startup_phase"], "initial_runtime_preparing");
}

#[test]
fn backend_rejects_old_error_payload_versions() {
    let error = BackendError::new("backend_starting", true, "starting")
        .with_error_version(DEVENV_BACKEND_ERROR_VERSION - 1)
        .with_request_id("request-1");
    let response = BackendResponseMessage::new("request-1", BackendResponse::Error(error.clone()));
    assert!(response.validate().is_err());
    let error = error.with_error_version(DEVENV_BACKEND_ERROR_VERSION);
    assert!(
        BackendResponseMessage::new("request-1", BackendResponse::Error(error))
            .validate()
            .is_ok()
    );
}

#[test]
fn materialization_key_digest_is_stable_and_context_sensitive() {
    let identity = EffectiveIdentity::root();
    let key = dev_env_model::MaterializationKeyBuilder::new(
        Generation::INITIAL,
        "/workspace",
        "/workspace/src",
        "bash",
        identity.clone(),
    )
    .with_config_fingerprint([7; 32])
    .with_runtime_inputs(BTreeMap::from([("MODE".to_owned(), "dev".to_owned())]))
    .with_ambient_environment(BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]))
    .with_provider_detect_fingerprints(BTreeMap::from([("devbox".to_owned(), [9; 32])]))
    .build()
    .unwrap();
    assert_eq!(key.fingerprint().unwrap(), key.digest().unwrap());

    let normalized = dev_env_model::MaterializationKeyBuilder::new(
        Generation::INITIAL,
        "/workspace/./src/..",
        "/workspace/src/../src",
        "bash",
        EffectiveIdentity::root(),
    )
    .with_config_fingerprint([7; 32])
    .with_runtime_inputs(BTreeMap::from([("MODE".to_owned(), "dev".to_owned())]))
    .with_ambient_environment(BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]))
    .with_provider_detect_fingerprints(BTreeMap::from([("devbox".to_owned(), [9; 32])]))
    .build()
    .unwrap();
    assert_eq!(
        key.fingerprint().unwrap(),
        normalized.fingerprint().unwrap()
    );

    let changed = dev_env_model::MaterializationKeyBuilder::new(
        Generation::INITIAL,
        "/workspace",
        "/workspace/src",
        "bash",
        identity,
    )
    .with_config_fingerprint([7; 32])
    .with_runtime_inputs(BTreeMap::from([("MODE".to_owned(), "prod".to_owned())]))
    .with_ambient_environment(BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]))
    .with_provider_detect_fingerprints(BTreeMap::from([("devbox".to_owned(), [9; 32])]))
    .build()
    .unwrap();
    assert_ne!(key.fingerprint().unwrap(), changed.fingerprint().unwrap());
}
