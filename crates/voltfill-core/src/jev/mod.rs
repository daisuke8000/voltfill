pub use jev_client::*;
pub mod transport;

pub(crate) fn entry(value: &Content, nullable: bool) -> bool {
    match value {
        Content::Null => nullable,
        Content::Text(s) => !s.trim().is_empty(),
        Content::Array(xs) => !xs.is_empty(),
        Content::Object(xs) => !xs.is_empty(),
        _ => false,
    }
}

pub(crate) fn fits_in_state(key: &str, value: &Content) -> bool {
    fn check(value: &Content, depth: usize, limit: usize) -> bool {
        if depth > limit {
            return false;
        }
        match value {
            Content::Float(n) => n.is_finite(),
            Content::Array(xs) => xs.iter().all(|x| check(x, depth + 1, limit)),
            Content::Object(xs) => xs.values().all(|x| check(x, depth + 1, limit)),
            _ => true,
        }
    }
    let limits = Limits::default();
    let state = Content::Object(std::collections::BTreeMap::from([(
        key.to_string(),
        value.clone(),
    )]));
    check(&state, 1, limits.max_depth)
        && serde_json::to_vec(&std::collections::BTreeMap::from([("state", &state)]))
            .is_ok_and(|bytes| json::parse(&bytes, limits).is_ok())
}
