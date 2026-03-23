pub fn escape_baml_description(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}
