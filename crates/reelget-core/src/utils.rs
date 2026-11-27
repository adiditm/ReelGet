use scraper::{Html, Selector};
use serde_json::Value;

/// Sanitize a string into a safe cross-platform filename stem (macOS, Linux, Windows, Android).
pub fn sanitize_filename(input: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(input.len().min(120));
    let mut last_was_space = false;

    for ch in input.chars() {
        if ch.is_control() {
            continue;
        }
        let mapped = match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_whitespace() => ' ',
            c => c,
        };
        if mapped == ' ' {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(mapped);
            last_was_space = false;
        }
        if out.len() >= 100 {
            break;
        }
    }

    let trimmed = out.trim().trim_matches('.').trim().to_string();
    if trimmed.is_empty() {
        return fallback.to_string();
    }

    // Guard against Windows reserved device names
    let upper = trimmed.to_ascii_uppercase();
    let is_reserved = matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if is_reserved {
        format!("_{trimmed}")
    } else {
        trimmed
    }
}

/// Extract all `<script type="application/json">` payloads parsed as `serde_json::Value`.
pub fn extract_json_scripts(html: &str) -> Vec<Value> {
    let doc = Html::parse_document(html);
    let Ok(sel) = Selector::parse(r#"script[type="application/json"]"#) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for el in doc.select(&sel) {
        let text: String = el.text().collect();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
            values.push(val);
        }
    }
    values
}

/// Extract a `<script id="...">` JSON payload parsed as `serde_json::Value`.
pub fn extract_json_script_by_id(html: &str, id: &str) -> Option<Value> {
    let doc = Html::parse_document(html);
    let selector_str = format!(r#"script#{id}"#);
    let sel = Selector::parse(&selector_str).ok()?;
    for el in doc.select(&sel) {
        let text: String = el.text().collect();
        let trimmed = text.trim();
        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
            return Some(val);
        }
    }
    None
}

/// Recursively walk a JSON value and return the first object matching `predicate`.
pub fn find_json_object<'a, F>(val: &'a Value, predicate: &F) -> Option<&'a serde_json::Map<String, Value>>
where
    F: Fn(&serde_json::Map<String, Value>) -> bool,
{
    match val {
        Value::Object(map) => {
            if predicate(map) {
                return Some(map);
            }
            for v in map.values() {
                if let Some(found) = find_json_object(v, predicate) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for v in arr {
                if let Some(found) = find_json_object(v, predicate) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Recursively collect all JSON objects matching `predicate`.
pub fn collect_json_objects<'a, F>(
    val: &'a Value,
    predicate: &F,
    out: &mut Vec<&'a serde_json::Map<String, Value>>,
) where
    F: Fn(&serde_json::Map<String, Value>) -> bool,
{
    match val {
        Value::Object(map) => {
            if predicate(map) {
                out.push(map);
            }
            for v in map.values() {
                collect_json_objects(v, predicate, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_json_objects(v, predicate, out);
            }
        }
        _ => {}
    }
}

/// Unescape common JSON/HTML escaped URLs (`\/`, `\u0026`, `&amp;`).
pub fn unescape_url(raw: &str) -> String {
    raw.replace(r"\/", "/")
        .replace(r"\u0026", "&")
        .replace(r"\u002F", "/")
        .replace(r"\u003D", "=")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("Hello: World/Test?", "vid"), "Hello_ World_Test_");
        assert_eq!(sanitize_filename("CON", "vid"), "_CON");
        assert_eq!(sanitize_filename("   ...   ", "fallback"), "fallback");
    }
}
