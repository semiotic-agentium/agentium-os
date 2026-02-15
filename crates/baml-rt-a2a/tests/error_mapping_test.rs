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
fn error_mapping_table_driven() {
    let cases: &[(BamlRtError, &str, bool)] = &[
        (
            BamlRtError::InvalidArgument("bad param".into()),
            "invalid_argument",
            false,
        ),
        (
            BamlRtError::FunctionNotFound("foo".into()),
            "function_not_found",
            false,
        ),
        (
            BamlRtError::ToolExecution("timeout".into()),
            "tool_execution",
            true,
        ),
        (
            BamlRtError::ProvenanceContextRead {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "test",
                )),
            },
            "provenance",
            true,
        ),
        (BamlRtError::QuickJs("script error".into()), "quickjs", true),
        (
            BamlRtError::Configuration("missing key".into()),
            "configuration",
            false,
        ),
        (
            BamlRtError::ToolRegistration("duplicate".into()),
            "tool_registration",
            false,
        ),
    ];

    for (err, expected_classifier, expected_retryable) in cases {
        assert_retryable_classifier(err, expected_classifier, *expected_retryable);
    }
}
