//! Tests for A2A error mapping. I4: every mapped error includes retryable and classifier.

use baml_rt_a2a::error_mapping;
use baml_rt_core::BamlRtError;

fn assert_retryable_classifier(
    error: &BamlRtError,
    expected_classifier: &str,
    expected_retryable: bool,
) {
    let m = error_mapping::map_error(error);
    assert_eq!(m.classifier, expected_classifier, "classifier");
    assert_eq!(m.retryable, expected_retryable, "retryable");
    let data = m.data.as_ref().expect("data present");
    assert_eq!(
        data.get("classifier").and_then(|v| v.as_str()),
        Some(expected_classifier),
        "data.classifier"
    );
    assert_eq!(
        data.get("retryable").and_then(|v| v.as_bool()),
        Some(expected_retryable),
        "data.retryable"
    );
}

#[test]
fn error_mapping_invalid_argument_has_retryable_false() {
    let err = BamlRtError::InvalidArgument("bad param".into());
    assert_retryable_classifier(&err, "invalid_argument", false);
}

#[test]
fn error_mapping_function_not_found_has_retryable_false() {
    let err = BamlRtError::FunctionNotFound("foo".into());
    assert_retryable_classifier(&err, "function_not_found", false);
}

#[test]
fn error_mapping_tool_execution_has_retryable_true() {
    let err = BamlRtError::ToolExecution("timeout".into());
    assert_retryable_classifier(&err, "tool_execution", true);
}

#[test]
fn error_mapping_provenance_context_read_has_retryable_true() {
    let err = BamlRtError::ProvenanceContextRead {
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "test",
        )),
    };
    assert_retryable_classifier(&err, "provenance", true);
}

#[test]
fn error_mapping_quickjs_has_retryable_true() {
    let err = BamlRtError::QuickJs("script error".into());
    assert_retryable_classifier(&err, "quickjs", true);
}

#[test]
fn error_mapping_configuration_has_retryable_false() {
    let err = BamlRtError::Configuration("missing key".into());
    assert_retryable_classifier(&err, "configuration", false);
}

#[test]
fn error_mapping_tool_registration_has_retryable_false() {
    let err = BamlRtError::ToolRegistration("duplicate".into());
    assert_retryable_classifier(&err, "tool_registration", false);
}
