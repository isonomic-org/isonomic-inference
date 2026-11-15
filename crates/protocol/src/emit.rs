//! Shared pieces of emit: options, blob lookup, and identifier mapping.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::conversation::MediaRef;
use crate::diag::{Diag, DiagKind, ProtocolError};
use crate::limits::Limits;

/// Where emit gets the bytes of a blob reference. The engine resolves blobs;
/// this crate only encodes what it is handed.
pub trait BlobStore {
    fn get(&self, id: &str) -> Option<&[u8]>;
}

/// A blob store with nothing in it.
pub struct NoBlobs;

impl BlobStore for NoBlobs {
    fn get(&self, _id: &str) -> Option<&[u8]> {
        None
    }
}

impl BlobStore for BTreeMap<String, Vec<u8>> {
    fn get(&self, id: &str) -> Option<&[u8]> {
        BTreeMap::get(self, id).map(Vec::as_slice)
    }
}

pub struct EmitOptions<'a> {
    /// Ask the server to stream its reply.
    pub stream: bool,
    /// The model to name when the conversation names none.
    pub default_model: Option<&'a str>,
    /// The messages format requires an output limit; this fills the gap.
    pub default_max_tokens: u32,
    /// Send earlier reasoning back to a chat-format server in the
    /// `reasoning_content` field, which local servers accept. Off by
    /// default: other servers may reject the field.
    pub chat_reasoning_history: bool,
    pub blobs: &'a dyn BlobStore,
    pub limits: Limits,
}

impl Default for EmitOptions<'_> {
    fn default() -> Self {
        Self {
            stream: false,
            default_model: None,
            default_max_tokens: 4096,
            chat_reasoning_history: false,
            blobs: &NoBlobs,
            limits: Limits::default(),
        }
    }
}

/// An emitted request with every diagnostic its translation raised.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Emitted<R> {
    pub(crate) request: R,
    pub(crate) diags: Vec<Diag>,
}

/// Inline media, resolved and encoded.
pub(crate) enum Media {
    Url(String),
    Inline { media_type: String, base64: String },
}

pub(crate) fn resolve(source: &MediaRef, opts: &EmitOptions<'_>) -> Result<Media, ProtocolError> {
    match source {
        MediaRef::Url { url } => Ok(Media::Url(url.clone())),
        MediaRef::Blob { id, media_type } => {
            let bytes = opts
                .blobs
                .get(id)
                .ok_or_else(|| ProtocolError::MissingBlob(id.clone()))?;
            if bytes.len() > opts.limits.max_inline_bytes {
                return Err(ProtocolError::Limit {
                    what: "inline blob bytes",
                    limit: opts.limits.max_inline_bytes,
                });
            }
            Ok(Media::Inline {
                media_type: media_type.clone(),
                base64: base64(bytes),
            })
        }
    }
}

/// Standard base64 with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.len();
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= n {
                out.push(ALPHABET[((v >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Rewrite an identifier to `[A-Za-z0-9_-]+`, the shape the messages format
/// requires for tool call IDs. The same input always maps to the same output,
/// so a call and its result stay paired.
pub(crate) fn safe_id(id: &str, at: String, diags: &mut Vec<Diag>) -> String {
    let ok = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if !id.is_empty() && id.chars().all(ok) {
        return id.to_string();
    }
    let mut mapped: String = id.chars().map(|c| if ok(c) { c } else { '_' }).collect();
    if mapped.is_empty() {
        mapped.push('_');
    }
    diags.push(Diag::new(
        DiagKind::IdMapped,
        at,
        format!("call id {id:?} sent as {mapped:?}"),
    ));
    mapped
}

/// Write the format's extension fields back into a request object. A field
/// the hub already emitted wins, with a diagnostic.
pub(crate) fn merge_ext(
    target: &mut Map<String, Value>,
    ext: Option<&Map<String, Value>>,
    diags: &mut Vec<Diag>,
) {
    for (key, value) in ext.into_iter().flatten() {
        if target.contains_key(key) {
            diags.push(Diag::new(
                DiagKind::Dropped,
                format!("ext.{key}"),
                "extension field collides with a field the hub sets",
            ));
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, want) in cases {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }

    #[test]
    fn unsafe_ids_are_mapped_with_a_diagnostic() {
        let mut diags = Vec::new();
        assert_eq!(safe_id("call_1", "x".into(), &mut diags), "call_1");
        assert!(diags.is_empty());
        assert_eq!(safe_id("call:1.a", "x".into(), &mut diags), "call_1_a");
        assert_eq!(diags[0].kind, DiagKind::IdMapped);
    }
}
