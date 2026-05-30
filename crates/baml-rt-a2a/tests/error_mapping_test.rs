// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![recursion_limit = "256"]

//! Tests for A2A error mapping. I4: every mapped error includes retryable and classifier.

use baml_rt_a2a::error_mapping;
use baml_rt_core::{BamlRtError, ClassifiedToolError, Retryability, semantics::ErrorDisposition};

fn assert_retryable_classifier(
    error: &BamlRtError,
    expected_classifier: &str,
    expected_retryable: Retryability,
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
        Some(expected_retryable.is_retryable()),
        "data.retryable"
    );
    assert!(
        data.get("error_disposition").is_some(),
        "data.error_disposition"
    );
}

#[test]
fn error_mapping_table_driven() {
    let cases: &[(BamlRtError, &str, Retryability)] = &[
        (
            BamlRtError::InvalidArgument("bad param".into()),
            "invalid_argument",
            Retryability::Permanent,
        ),
        (
            BamlRtError::FunctionNotFound("foo".into()),
            "function_not_found",
            Retryability::Permanent,
        ),
        (
            BamlRtError::ToolExecution("timeout".into()),
            "tool_execution",
            Retryability::Permanent,
        ),
        (
            BamlRtError::ToolExecution("HTTP 401 unauthorized".into()),
            "tool_execution",
            Retryability::Permanent,
        ),
        (
            BamlRtError::ToolClassified(ClassifiedToolError {
                code: "vendor_rate_limited".into(),
                disposition: ErrorDisposition::HostRetriable,
                message: "slow down".into(),
                hint: None,
                retry_after_ms: None,
            }),
            "tool_classified",
            Retryability::Retryable,
        ),
        (
            BamlRtError::ProvenanceContextRead {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "test",
                )),
            },
            "provenance",
            Retryability::Retryable,
        ),
        (
            BamlRtError::QuickJs("script error".into()),
            "quickjs",
            Retryability::Retryable,
        ),
        (
            BamlRtError::Configuration("missing key".into()),
            "configuration",
            Retryability::Permanent,
        ),
        (
            BamlRtError::ToolRegistration("duplicate".into()),
            "tool_registration",
            Retryability::Permanent,
        ),
    ];

    for (err, expected_classifier, expected_retryable) in cases {
        assert_retryable_classifier(err, expected_classifier, *expected_retryable);
    }
}
