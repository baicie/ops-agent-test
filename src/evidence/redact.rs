use serde_json::{Map, Value};

const REDACTED: &str = "[REDACTED]";

pub fn redact_json(value: &Value) -> (Value, bool) {
    let mut redacted_any = false;
    let redacted = redact_value(value, &mut redacted_any);
    (redacted, redacted_any)
}

pub fn redact_text(input: &str) -> (String, bool) {
    let mut output = input.to_owned();
    let mut redacted_any = false;
    redacted_any |= redact_pem(&mut output);
    redacted_any |= replace_prefixed(&mut output, "sk-", 20);
    redacted_any |= replace_prefixed(&mut output, "AKIA", 16);
    redacted_any |= redact_bearer(&mut output);
    redacted_any |= redact_assignment(&mut output, "password");
    redacted_any |= redact_assignment(&mut output, "secret");
    redacted_any |= redact_assignment(&mut output, "api_key");
    redacted_any |= redact_assignment(&mut output, "api-key");
    redacted_any |= redact_assignment(&mut output, "token");
    (output, redacted_any)
}

fn redact_value(value: &Value, redacted_any: &mut bool) -> Value {
    match value {
        Value::String(text) => {
            let (redacted, changed) = redact_text(text);
            *redacted_any |= changed;
            Value::String(redacted)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_value(item, redacted_any))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                if is_sensitive_key(key) {
                    *redacted_any = true;
                    out.insert(key.clone(), Value::String(REDACTED.into()));
                } else {
                    out.insert(key.clone(), redact_value(child, redacted_any));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "password" | "secret" | "token" | "api_key" | "apikey" | "authorization" | "private_key"
    )
}

fn redact_pem(output: &mut String) -> bool {
    let start = output.find("-----BEGIN ");
    let end = output.find("-----END ");
    if let (Some(start), Some(end)) = (start, end) {
        let end = output[end..]
            .find("-----")
            .map(|offset| end + offset + 5)
            .unwrap_or(output.len());
        output.replace_range(start..end, "[REDACTED_PRIVATE_KEY]");
        return true;
    }
    false
}

fn replace_prefixed(output: &mut String, prefix: &str, min_rest: usize) -> bool {
    let mut changed = false;
    let mut search_from = 0;
    while let Some(relative) = output[search_from..].find(prefix) {
        let start = search_from + relative;
        let rest_start = start + prefix.len();
        let rest_len = output[rest_start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .count();
        if rest_len >= min_rest {
            let end = rest_start
                + output[rest_start..]
                    .char_indices()
                    .nth(rest_len)
                    .map(|(index, _)| index)
                    .unwrap_or(output[rest_start..].len());
            output.replace_range(start..end, REDACTED);
            changed = true;
            search_from = start + REDACTED.len();
        } else {
            search_from = rest_start;
        }
    }
    changed
}

fn redact_bearer(output: &mut String) -> bool {
    let lower = output.to_ascii_lowercase();
    if let Some(index) = lower.find("bearer ") {
        let rest_start = index + "bearer ".len();
        let rest_len = output[rest_start..]
            .chars()
            .take_while(|ch| !ch.is_whitespace())
            .count();
        if rest_len > 8 {
            let end = rest_start
                + output[rest_start..]
                    .char_indices()
                    .nth(rest_len)
                    .map(|(index, _)| index)
                    .unwrap_or(output[rest_start..].len());
            output.replace_range(index..end, "Bearer [REDACTED]");
            return true;
        }
    }
    false
}

fn redact_assignment(output: &mut String, key: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    let mut changed = false;
    let mut search_from = 0;
    while let Some(relative) = lower[search_from..].find(key) {
        let start = search_from + relative;
        let after_key = start + key.len();
        let Some(rest) = output.get(after_key..) else {
            break;
        };
        let trimmed = rest.trim_start();
        let skipped = rest.len() - trimmed.len();
        let Some(first) = trimmed.chars().next() else {
            break;
        };
        if first != '=' && first != ':' {
            search_from = after_key;
            continue;
        }
        let value_start = after_key + skipped + 1;
        let value = output.get(value_start..).unwrap_or("");
        let value_trimmed = value.trim_start();
        let value_skip = value.len() - value_trimmed.len();
        let value_len = value_trimmed
            .chars()
            .take_while(|ch| !ch.is_whitespace() && *ch != ',' && *ch != ';' && *ch != '"')
            .count();
        if value_len > 0 {
            let abs_start = value_start + value_skip;
            let abs_end = abs_start
                + value_trimmed
                    .char_indices()
                    .nth(value_len)
                    .map(|(index, _)| index)
                    .unwrap_or(value_trimmed.len());
            output.replace_range(abs_start..abs_end, REDACTED);
            changed = true;
            break;
        }
        search_from = after_key;
    }
    changed
}
