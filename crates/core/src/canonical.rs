//! Canonical JSON: the one byte string everyone signs and hashes.
//!
//! Object keys are sorted. No whitespace. Same input, same bytes, on every
//! machine and every version. Signatures and chain hashes depend on this.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Serialize a JSON value with sorted keys and no whitespace.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_value(v, &mut out);
    out
}

fn write_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => {
            // serde_json's string escaping is deterministic.
            out.push_str(&serde_json::to_string(s).expect("string is always serializable"));
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string is always serializable"));
                out.push(':');
                write_value(&map[*k], out);
            }
            out.push('}');
        }
    }
}

/// `sha256:<hex>` of the canonical form of a JSON value.
pub fn sha256_json(v: &Value) -> String {
    sha256_bytes(canonical_json(v).as_bytes())
}

/// `sha256:<hex>` of raw bytes.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", data_encoding::HEXLOWER.encode(&digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_and_compact() {
        let v = json!({"b": 1, "a": {"z": [1, 2, {"y": null}], "m": "x"}});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"m":"x","z":[1,2,{"y":null}]},"b":1}"#
        );
    }

    #[test]
    fn hash_is_stable() {
        let v = json!({"text": "hi"});
        assert_eq!(sha256_json(&v), sha256_json(&json!({"text": "hi"})));
        assert!(sha256_json(&v).starts_with("sha256:"));
    }
}
