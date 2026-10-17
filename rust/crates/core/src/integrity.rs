//! Small deterministic integrity primitives shared by feature slices.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Returns a SHA-256 identity for JSON which is stable across object-key order.
/// PostgreSQL `JSONB` normalizes objects, so publication and worker code must
/// not use a serializer's incidental map ordering as an immutable snapshot
/// identity. Arrays retain their order; each value is type-tagged and string
/// components are length-delimited to avoid concatenation ambiguity.
#[must_use]
pub fn canonical_json_sha256(value: &Value) -> String {
    let mut hasher = Sha256::new();
    update_json_checksum(&mut hasher, value);
    hex::encode(hasher.finalize())
}

fn update_json_checksum(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => {
            hasher.update(b"n");
        }
        Value::Bool(boolean) => {
            hasher.update(if *boolean { b"t" } else { b"f" });
        }
        Value::Number(number) => {
            update_component(hasher, b"#", number.to_string().as_bytes());
        }
        Value::String(string) => {
            update_component(hasher, b"s", string.as_bytes());
        }
        Value::Array(values) => {
            hasher.update(b"[");
            for value in values {
                update_json_checksum(hasher, value);
            }
            hasher.update(b"]");
        }
        Value::Object(values) => {
            hasher.update(b"{");
            let mut sorted = values.iter().collect::<Vec<_>>();
            sorted.sort_unstable_by_key(|(key, _)| *key);
            for (key, value) in sorted {
                update_component(hasher, b"k", key.as_bytes());
                update_json_checksum(hasher, value);
            }
            hasher.update(b"}");
        }
    }
}

fn update_component(hasher: &mut Sha256, tag: &[u8], value: &[u8]) {
    hasher.update(tag);
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::canonical_json_sha256;

    #[test]
    fn canonical_hash_ignores_object_key_order_but_not_array_order() {
        assert_eq!(
            canonical_json_sha256(&json!({"a": [1, 2], "b": {"x": true}})),
            canonical_json_sha256(&json!({"b": {"x": true}, "a": [1, 2]}))
        );
        assert_ne!(
            canonical_json_sha256(&json!([1, 2])),
            canonical_json_sha256(&json!([2, 1]))
        );
    }
}
