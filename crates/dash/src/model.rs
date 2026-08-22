//! Query display names. Agent stores raw slugs; pricing looks up raw slugs.

/// Fold effort / thinking, Cursor channel prefix, and Grok `-build`.
/// Keep Fast vs standard and the family SKU.
pub fn display_model(name: &str) -> String {
    let trimmed = name.trim();
    let body = strip_prefix_ci(trimmed, "cursor-");
    let parts: Vec<&str> = body
        .split('-')
        .filter(|p| !p.is_empty() && !is_folded_token(p))
        .collect();
    if parts.is_empty() {
        trimmed.to_string()
    } else {
        parts.join("-")
    }
}

fn strip_prefix_ci<'a>(name: &'a str, prefix: &str) -> &'a str {
    if name.len() >= prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix) {
        &name[prefix.len()..]
    } else {
        name
    }
}

fn is_folded_token(part: &str) -> bool {
    matches!(
        part.to_ascii_lowercase().as_str(),
        "xhigh" | "high" | "medium" | "low" | "max" | "thinking" | "build"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_effort_channel_and_build_keeps_fast() {
        assert_eq!(display_model("cursor-grok-4.6-xhigh-fast"), "grok-4.6-fast");
        assert_eq!(display_model("cursor-grok-4.6-high"), "grok-4.6");
        assert_eq!(display_model("grok-4.6-build"), "grok-4.6");
        assert_eq!(display_model("composer-2.5-medium"), "composer-2.5");
        assert_eq!(display_model("composer-2.5-fast"), "composer-2.5-fast");
        assert_eq!(display_model("gpt-5.6-sol-max"), "gpt-5.6-sol");
        assert_eq!(display_model("gpt-5.6-sol-fast"), "gpt-5.6-sol-fast");
        assert_eq!(
            display_model("claude-fable-5-thinking-max"),
            "claude-fable-5"
        );
        assert_eq!(display_model("claude-opus-5-thinking"), "claude-opus-5");
        assert_eq!(display_model("claude-opus-5"), "claude-opus-5");
    }
}
