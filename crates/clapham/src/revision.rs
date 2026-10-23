use std::collections::BTreeMap;
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) fn digest(domain: &[u8], value: impl Serialize) -> String {
    let value = serde_json::to_value(value).expect("revision inputs contain only JSON values");
    let canonical = canonicalise(value);
    let mut hasher = Sha256::new();
    hasher.update(domain);
    serde_json::to_writer(&mut HashWriter(&mut hasher), &canonical)
        .expect("JSON values are serialisable");
    format!("sha256:{:x}", hasher.finalize())
}

fn canonicalise(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            // Downstream features may enable serde_json/preserve_order.
            let sorted: BTreeMap<_, _> = object
                .into_iter()
                .map(|(key, value)| (key, canonicalise(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(array) => Value::Array(array.into_iter().map(canonicalise).collect()),
        value => value,
    }
}

struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
