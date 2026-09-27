use dev_env_model::{
    decode_backend_request, encode_backend_request, read_backend_request, write_backend_request,
    BackendError, BackendRequest, BackendRequestMessage, BackendResponse, BackendResponseMessage,
    EffectiveIdentity, Generation, IdentityRequest, MaterializationKey, RequestContext,
    RequestMode, DEVENV_BACKEND_ERROR_VERSION, DEVENV_BACKEND_PROTOCOL_VERSION,
};
use std::collections::BTreeMap;

#[test]
fn backend_v3_request_round_trips_and_rejects_invalid_context() {
    let context = RequestContext::new("request-1", RequestMode::Exec, "/workspace", "bash")
        .with_identity(IdentityRequest::Peer)
        .with_runtime_input("DEVBOX_AUTO_INIT", "never")
        .with_ambient_environment(BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]));
    let original = BackendRequestMessage::new("request-1", BackendRequest::Prepare { context });
    let encoded = encode_backend_request(&original).unwrap();
    let decoded = decode_backend_request(&encoded).unwrap();
    assert_eq!(decoded, original);
    assert_eq!(decoded.protocol, DEVENV_BACKEND_PROTOCOL_VERSION);
    assert_eq!(DEVENV_BACKEND_PROTOCOL_VERSION, 3);

    let invalid = BackendRequestMessage::new(
        "request-2",
        BackendRequest::Prepare {
            context: RequestContext::new("request-2", RequestMode::Exec, "relative", "bash"),
        },
    );
    assert!(encode_backend_request(&invalid).is_err());

    let mut frame = Vec::new();
    write_backend_request(&mut frame, &original).unwrap();
    assert_eq!(
        read_backend_request(&mut std::io::Cursor::new(frame)).unwrap(),
        original
    );
}

#[test]
fn backend_rejects_old_error_payload_versions() {
    let mut error = BackendError {
        error_version: DEVENV_BACKEND_ERROR_VERSION - 1,
        class: "backend_busy".to_owned(),
        retryable: true,
        message: "busy".to_owned(),
        generation: None,
        provider_id: None,
        operation: None,
        request_id: Some("request-1".to_owned()),
    };
    let response = BackendResponseMessage::new("request-1", BackendResponse::Error(error.clone()));
    assert!(response.validate().is_err());
    error.error_version = DEVENV_BACKEND_ERROR_VERSION;
    assert!(
        BackendResponseMessage::new("request-1", BackendResponse::Error(error))
            .validate()
            .is_ok()
    );
}

#[test]
fn materialization_key_digest_is_stable_and_context_sensitive() {
    let identity = EffectiveIdentity::root();
    let key = MaterializationKey::new(
        Generation::INITIAL,
        [7; 32],
        "/workspace",
        "/workspace/src",
        "bash",
        identity.clone(),
        BTreeMap::from([("MODE".to_owned(), "dev".to_owned())]),
        BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]),
        BTreeMap::from([("devbox".to_owned(), [9; 32])]),
    )
    .unwrap();
    assert_eq!(key.fingerprint().unwrap(), key.digest().unwrap());

    let normalized = MaterializationKey::new(
        Generation::INITIAL,
        [7; 32],
        "/workspace/./src/..",
        "/workspace/src/../src",
        "bash",
        EffectiveIdentity::root(),
        BTreeMap::from([("MODE".to_owned(), "dev".to_owned())]),
        BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]),
        BTreeMap::from([("devbox".to_owned(), [9; 32])]),
    )
    .unwrap();
    assert_eq!(
        key.fingerprint().unwrap(),
        normalized.fingerprint().unwrap()
    );

    let changed = MaterializationKey::new(
        Generation::INITIAL,
        [7; 32],
        "/workspace",
        "/workspace/src",
        "bash",
        identity,
        BTreeMap::from([("MODE".to_owned(), "prod".to_owned())]),
        BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]),
        BTreeMap::from([("devbox".to_owned(), [9; 32])]),
    )
    .unwrap();
    assert_ne!(key.fingerprint().unwrap(), changed.fingerprint().unwrap());
}
