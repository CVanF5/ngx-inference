// Model extraction utilities for BBR (Body-Based Routing)
// Separated for easier unit testing without nginx dependencies

use serde_json::Value;

/// Extract model name from JSON request body following OpenAI API specification
pub fn extract_model_from_body(body: &[u8]) -> Option<String> {
    // Parse JSON to extract model field following OpenAI API specification
    if let Ok(json_str) = std::str::from_utf8(body) {
        if let Ok(json) = serde_json::from_str::<Value>(json_str) {
            return json
                .get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_model_from_body_valid_model() {
        let json_body = r#"{"model": "gpt-4", "prompt": "Hello world"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_extract_model_from_body_complex_model_name() {
        let json_body = r#"{"model": "claude-3-opus-20240229", "messages": [{"role": "user", "content": "test"}]}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, Some("claude-3-opus-20240229".to_string()));
    }

    #[test]
    fn test_extract_model_from_body_no_model_field() {
        let json_body = r#"{"prompt": "Hello world", "temperature": 0.7}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_null_model() {
        let json_body = r#"{"model": null, "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_empty_string_model() {
        let json_body = r#"{"model": "", "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_numeric_model() {
        let json_body = r#"{"model": 123, "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_boolean_model() {
        let json_body = r#"{"model": true, "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_array_model() {
        let json_body = r#"{"model": ["gpt-4"], "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_object_model() {
        let json_body = r#"{"model": {"name": "gpt-4"}, "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_invalid_json() {
        let invalid_json = b"not json at all";
        let result = extract_model_from_body(invalid_json);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_malformed_json() {
        let malformed_json = b"{\"model\": \"gpt-4\", \"prompt\":}";
        let result = extract_model_from_body(malformed_json);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_empty_body() {
        let empty_body = b"";
        let result = extract_model_from_body(empty_body);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_non_utf8() {
        let non_utf8 = &[0xFF, 0xFE, 0xFD];
        let result = extract_model_from_body(non_utf8);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_model_from_body_whitespace_only_model() {
        let json_body = r#"{"model": "   ", "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, Some("   ".to_string())); // Whitespace-only strings are preserved
    }

    #[test]
    fn test_extract_model_from_body_unicode_model() {
        let json_body = r#"{"model": "模型-4", "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, Some("模型-4".to_string()));
    }

    #[test]
    fn test_extract_model_from_body_special_chars_model() {
        let json_body = r#"{"model": "gpt-4-@#$%^&*()", "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, Some("gpt-4-@#$%^&*()".to_string()));
    }

    #[test]
    fn test_extract_model_from_body_nested_objects() {
        let json_body = r#"{"request": {"model": "gpt-4"}, "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None); // Should only look at top-level "model" field
    }

    #[test]
    fn test_extract_model_from_body_case_sensitive() {
        let json_body = r#"{"Model": "gpt-4", "prompt": "test"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, None); // "Model" vs "model" - case sensitive
    }

    #[test]
    fn test_extract_model_from_body_multiple_models() {
        let json_body = r#"{"model": "first", "prompt": "test", "fallback_model": "second"}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, Some("first".to_string())); // Should take the first "model" field
    }

    #[test]
    fn test_extract_model_from_body_large_json() {
        let large_content = "x".repeat(1000);
        let json_body = format!(
            r#"{{"model": "gpt-4", "large_field": "{}", "prompt": "test"}}"#,
            large_content
        );
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_extract_model_from_body_deeply_nested() {
        let json_body =
            r#"{"model": "gpt-4", "nested": {"level1": {"level2": {"level3": "deep"}}}}"#;
        let result = extract_model_from_body(json_body.as_bytes());
        assert_eq!(result, Some("gpt-4".to_string()));
    }
}
