// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_tools::json_schema_value;
use baml_rt_tools_claude::{
    ClaudeCompletion, ClaudeEventDto, ClaudeToolNextOutput, ClaudeToolOpenInput,
    ClaudeToolSendInput, ClaudeUserContentBlockDto, ContextItem, ReviewDecision, UserInput,
};
use serde_json::json;

#[test]
fn claude_open_input_defaults_to_none_workspace() {
    let open: ClaudeToolOpenInput = serde_json::from_value(json!({})).expect("valid open input");
    assert_eq!(open.workspace, None);
}

#[test]
fn claude_send_input_roundtrip() {
    let input = ClaudeToolSendInput {
        prompt: Some("analyze this".to_string()),
        content: Some(vec![ClaudeUserContentBlockDto::ImageUrl {
            url: "https://example.com/image.png".to_string(),
        }]),
        user_input: None,
    };
    let value = serde_json::to_value(&input).expect("serialize");
    let decoded: ClaudeToolSendInput = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded.prompt.as_deref(), Some("analyze this"));
    assert_eq!(decoded.content.expect("content").len(), 1);
}

#[test]
fn claude_send_input_content_single_object_deserializes_like_one_element_array() {
    // BAML / LLM often emit one content block as an object, not a one-element array.
    let value = json!({
        "prompt": "go",
        "content": { "kind": "text", "text": "hello" }
    });
    let decoded: ClaudeToolSendInput = serde_json::from_value(value).expect("deserialize");
    let blocks = decoded.content.as_ref().expect("content");
    assert_eq!(blocks.len(), 1);
    assert!(matches!(
        &blocks[0],
        ClaudeUserContentBlockDto::Text { text } if text == "hello"
    ));
}

#[test]
fn claude_send_input_with_user_input_tool_approval_roundtrip() {
    // BAML-facing: userInput has no requestId.
    let input = ClaudeToolSendInput {
        prompt: Some("I approve the file write.".to_string()),
        content: None,
        user_input: Some(json!({
            "kind": "toolApproval",
            "approved": true,
            "reason": null
        })),
    };
    let value = serde_json::to_value(&input).expect("serialize");
    let decoded: ClaudeToolSendInput = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded.prompt.as_deref(), Some("I approve the file write."));
    let ui = decoded.user_input.as_ref().expect("user_input");
    assert_eq!(
        ui.get("kind").and_then(|v| v.as_str()),
        Some("toolApproval")
    );
    assert_eq!(ui.get("approved").and_then(|v| v.as_bool()), Some(true));
    assert!(
        ui.get("requestId").is_none(),
        "requestId must not exist in BAML-facing payload"
    );
}

#[test]
fn claude_next_output_serializes_completion_and_events() {
    let output = ClaudeToolNextOutput {
        events: vec![ClaudeEventDto::AssistantText {
            text: "hello".to_string(),
        }],
        completion: Some(ClaudeCompletion::InputRequired),
        history_context: None,
    };
    let value = serde_json::to_value(output).expect("serialize");
    assert_eq!(value["events"][0]["kind"], "assistant_text");
    assert_eq!(value["completion"], "INPUT_REQUIRED");
}

#[test]
fn claude_dto_schema_generation_is_valid() {
    let json = json_schema_value::<ClaudeToolNextOutput>();
    assert!(json.get("$schema").is_some());
}

// --- UserInput (Claude API input variant) ---

#[test]
fn user_input_text_roundtrip() {
    let input = UserInput::Text {
        text: "hello".to_string(),
    };
    let value = serde_json::to_value(&input).expect("serialize");
    assert_eq!(value["kind"], "text");
    assert_eq!(value["text"].as_str(), Some("hello"));
    let decoded: UserInput = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded.display_text(), "hello");
    assert!(!decoded.is_tool_approval());
}

#[test]
fn user_input_tool_approval_roundtrip() {
    // BAML-facing shape: no request_id (session matches pending request internally).
    let input = UserInput::ToolApproval {
        approved: true,
        modified_input: Some(json!({"path": "/tmp/foo"})),
        reason: Some("ok".to_string()),
    };
    let value = serde_json::to_value(&input).expect("serialize");
    assert_eq!(value["kind"].as_str(), Some("toolApproval"));
    assert!(
        value.get("requestId").is_none(),
        "requestId must not exist in BAML-facing output"
    );
    assert_eq!(value["approved"].as_bool(), Some(true));
    let decoded: UserInput = serde_json::from_value(value).expect("deserialize");
    assert!(decoded.is_tool_approval());
    assert_eq!(decoded.display_text(), "Approved: ok");
}

#[test]
fn user_input_tool_approval_rejected_no_reason() {
    let input = UserInput::ToolApproval {
        approved: false,
        modified_input: None,
        reason: None,
    };
    assert_eq!(input.display_text(), "Rejected");
    assert!(input.is_tool_approval());
}

#[test]
fn user_input_text_with_context_roundtrip() {
    let input = UserInput::TextWithContext {
        text: "see context".to_string(),
        context: vec![ContextItem {
            role: Some("system".to_string()),
            content: Some(json!("hint")),
            extra: std::collections::HashMap::new(),
        }],
    };
    let value = serde_json::to_value(&input).expect("serialize");
    assert_eq!(value["kind"], "textWithContext");
    let decoded: UserInput = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded.display_text(), "see context");
    assert!(!decoded.is_tool_approval());
}

#[test]
fn user_input_patch_approval_roundtrip() {
    let input = UserInput::PatchApproval {
        request_id: "patch-1".to_string(),
        decision: ReviewDecision::Approved,
    };
    let value = serde_json::to_value(&input).expect("serialize");
    assert_eq!(value["kind"], "patchApproval");
    let decoded: UserInput = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded.display_text(), "Approved");
    assert!(!decoded.is_tool_approval());
}

#[test]
fn user_input_get_history_and_list_mcp_tools() {
    let get_hist = UserInput::GetHistory;
    assert_eq!(get_hist.display_text(), "GetHistory");
    let value = serde_json::to_value(&get_hist).expect("serialize");
    assert_eq!(value["kind"], "getHistory");

    let list_tools = UserInput::ListMcpTools;
    assert_eq!(list_tools.display_text(), "ListMcpTools");
    let value = serde_json::to_value(&list_tools).expect("serialize");
    assert_eq!(value["kind"], "listMcpTools");
}
