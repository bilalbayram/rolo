use serde_json::Value;

/// Every `$ref` value in `value`.
fn refs<'a>(value: &'a Value, found: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value) {
                    ("$ref", Value::String(target)) => found.push(target),
                    _ => refs(value, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| refs(item, found)),
        _ => {}
    }
}

#[test]
fn every_ref_resolves() {
    let spec = serde_json::to_value(rolo::openapi()).unwrap();
    let mut found = Vec::new();
    refs(&spec, &mut found);
    assert!(!found.is_empty());
    let unresolved: Vec<&str> = found
        .into_iter()
        .filter(|target| {
            let pointer = target.strip_prefix('#').unwrap_or("not a local ref");
            spec.pointer(pointer).is_none()
        })
        .collect();
    assert!(unresolved.is_empty(), "unresolved: {unresolved:?}");
}
