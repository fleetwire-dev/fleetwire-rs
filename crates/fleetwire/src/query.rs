//! Query-parameter builders for GET requests (e.g. `vehicle_data` endpoints list).

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// Percent-encoding set for query keys/values and VIN path segments:
/// everything non-alphanumeric except `-_.~` (the URL "unreserved" set).
const FORM: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Structured query params. Never string-concatenated by callers.
#[derive(Debug, Clone, Default)]
pub struct QuerySpec {
    pairs: Vec<(String, String)>,
}

impl QuerySpec {
    /// New empty spec.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a key/value pair (value stringified).
    pub fn push(&mut self, key: &str, val: impl ToString) {
        self.pairs.push((key.to_string(), val.to_string()));
    }

    /// Percent-encoded `k=v&...` (empty string when no pairs).
    pub fn to_encoded(&self) -> String {
        self.pairs
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    utf8_percent_encode(k, FORM),
                    utf8_percent_encode(v, FORM)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
    }
}

/// Error encoding a VIN into a path segment.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum VinError {
    /// VIN contained a path-traversal, separator, scheme, or control character.
    #[error("unsafe VIN path segment")]
    Unsafe,
}

/// Encode a VIN as a single safe path segment (blocks traversal/scheme/authority).
pub fn encode_vin_segment(vin: &str) -> Result<String, VinError> {
    if vin.is_empty()
        || vin.contains('/')
        || vin.contains('\\')
        || vin.contains(':')
        || vin.contains('\0')
        || vin.contains("..")
    {
        return Err(VinError::Unsafe);
    }
    Ok(utf8_percent_encode(vin, FORM).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_encodes_and_joins() {
        let mut q = QuerySpec::new();
        q.push("page", 2u64);
        q.push("per_page", 100u64);
        assert_eq!(q.to_encoded(), "page=2&per_page=100");
        let e = QuerySpec::new();
        assert_eq!(e.to_encoded(), "");
    }
    #[test]
    fn query_percent_encodes_special() {
        let mut q = QuerySpec::new();
        q.push("endpoints", "location_data;charge_state");
        assert!(q.to_encoded().contains("%3B")); // ';' encoded
    }
    #[test]
    fn vin_rejects_traversal_and_scheme() {
        assert!(encode_vin_segment("../secrets").is_err());
        assert!(encode_vin_segment("a/b").is_err());
        assert!(encode_vin_segment("http://evil").is_err());
        assert!(encode_vin_segment("5YJ3E1EA7KF000000").is_ok());
    }
    #[test]
    fn query_percent_encodes_unicode_value() {
        let mut q = QuerySpec::new();
        q.push("name", "테슬라");
        let encoded = q.to_encoded();
        assert!(encoded.starts_with("name=%"));
        assert!(encoded.is_ascii());
    }
    #[test]
    fn vin_rejects_empty() {
        assert_eq!(encode_vin_segment(""), Err(VinError::Unsafe));
    }
    #[test]
    fn vin_rejects_nul_and_backslash() {
        assert!(encode_vin_segment("a\0b").is_err());
        assert!(encode_vin_segment("..\\secrets").is_err());
    }
    #[test]
    fn vin_percent_encodes_literal_percent() {
        // A raw '%' in the VIN must itself be escaped to "%25" — otherwise a
        // crafted VIN like "5YJ%2Fsecrets" (no literal '/' byte, so it passes
        // the separator check) could be misread as an encoded '/' by a
        // downstream percent-decoder, smuggling an extra path segment.
        let encoded = encode_vin_segment("5YJ%2Fsecrets").unwrap();
        assert_eq!(encoded, "5YJ%252Fsecrets");
        assert!(!encoded.contains('/'));
    }
}
