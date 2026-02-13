use serde_json::Value;
use std::io::{self, Read};

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).expect("read stdin");
    if input.trim().is_empty() {
        return;
    }

    let values = if let Ok(value) = serde_json::from_str::<Value>(&input) {
        flatten_values(value)
    } else {
        input
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .flat_map(flatten_values)
            .collect()
    };

    for value in values {
        let chunk = extract_chunk(&value).unwrap_or(value);
        if let Some(event) = chunk.get("event") {
            print_event(event);
            continue;
        }
        if let Some(message) = chunk.get("message") {
            print_message(message);
            continue;
        }
        if let Some(status) = chunk.get("statusUpdate") {
            print_status(status);
            continue;
        }
        if let Some(artifact) = chunk.get("artifactUpdate") {
            print_artifact(artifact);
            continue;
        }
    }
}

fn flatten_values(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        other => vec![other],
    }
}

fn extract_chunk(value: &Value) -> Option<Value> {
    let result = value.get("result")?.clone();
    Some(result.get("chunk").cloned().unwrap_or(result))
}

fn print_event(event: &Value) {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let task_id = event.get("taskId").and_then(Value::as_str).unwrap_or("-");
    let message_id = event
        .get("messageId")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let source = event.get("source").and_then(Value::as_str).unwrap_or("-");
    let mut extra = String::new();
    if let Some(tool) = event.get("tool") {
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("-");
        extra = format!(" tool={}", name);
    }
    println!(
        "event type={} taskId={} messageId={} source={}{}",
        event_type, task_id, message_id, source, extra
    );
}

fn print_message(message: &Value) {
    let text = message
        .get("parts")
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    println!("message text={}", text);
}

fn print_status(status: &Value) {
    let state = status
        .get("status")
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)
        .unwrap_or("-");
    println!("status state={}", state);
}

fn print_artifact(artifact: &Value) {
    let name = artifact
        .get("artifact")
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("-");
    println!("artifact name={}", name);
}
