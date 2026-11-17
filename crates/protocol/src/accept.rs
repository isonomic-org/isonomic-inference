// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared pieces of accept: the accepted request, and inline media turned
//! into blob references.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::conversation::{Conversation, MediaRef};
use crate::diag::{Diag, ProtocolError};
use crate::emit::BlobStore;
use crate::limits::Limits;

/// A wire request translated to the hub.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Accepted {
    pub conversation: Conversation,
    /// The caller asked for a streamed reply.
    pub stream: bool,
    /// The caller asked for token usage at the end of a stream (chat only;
    /// the messages format always reports it).
    pub stream_usage: bool,
    /// Inline media from the request, by the blob IDs the conversation
    /// refers to. The engine stores or resolves them; emit reads them back.
    pub blobs: BTreeMap<String, Blob>,
    pub diags: Vec<Diag>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

impl BlobStore for BTreeMap<String, Blob> {
    fn get(&self, id: &str) -> Option<&[u8]> {
        BTreeMap::get(self, id).map(|b| b.bytes.as_slice())
    }
}

/// Turns inline base64 data into numbered blobs, under the inline limit.
pub(crate) struct Inliner<'a> {
    pub(crate) limits: &'a Limits,
    pub(crate) blobs: BTreeMap<String, Blob>,
}

impl<'a> Inliner<'a> {
    pub(crate) fn new(limits: &'a Limits) -> Self {
        Self {
            limits,
            blobs: BTreeMap::new(),
        }
    }

    pub(crate) fn inline(
        &mut self,
        media_type: &str,
        base64: &str,
    ) -> Result<MediaRef, ProtocolError> {
        // Decoded size is at most three quarters of the text: check first.
        if base64.len() / 4 * 3 > self.limits.max_inline_bytes + 3 {
            return Err(ProtocolError::Limit {
                what: "inline blob bytes",
                limit: self.limits.max_inline_bytes,
            });
        }
        let bytes = decode_base64(base64)
            .ok_or_else(|| ProtocolError::Invalid("inline data is not base64".into()))?;
        if bytes.len() > self.limits.max_inline_bytes {
            return Err(ProtocolError::Limit {
                what: "inline blob bytes",
                limit: self.limits.max_inline_bytes,
            });
        }
        let id = format!("blob-{}", self.blobs.len() + 1);
        self.blobs.insert(
            id.clone(),
            Blob {
                media_type: media_type.to_string(),
                bytes,
            },
        );
        Ok(MediaRef::Blob {
            id,
            media_type: media_type.to_string(),
        })
    }

    /// A URL, or a `data:` URL turned into a blob.
    pub(crate) fn url(&mut self, url: &str) -> Result<MediaRef, ProtocolError> {
        match parse_data_url(url) {
            Some((media_type, data)) => self.inline(media_type, data),
            None => Ok(MediaRef::Url {
                url: url.to_string(),
            }),
        }
    }
}

/// `data:<media type>;base64,<data>` → (media type, data).
pub(crate) fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (head, data) = rest.split_once(',')?;
    let media_type = head.strip_suffix(";base64")?;
    Some((media_type, data))
}

/// Standard base64, padding optional, no whitespace.
pub(crate) fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // A single leftover character cannot encode a byte.
    (bits < 6).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_the_reference_vectors() {
        let cases = [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg", "foob"),
            ("Zm9vYmFy", "foobar"),
        ];
        for (input, want) in cases {
            assert_eq!(decode_base64(input).unwrap(), want.as_bytes());
        }
        assert!(decode_base64("Zm9v!").is_none());
        assert!(decode_base64("Z").is_none());
    }

    #[test]
    fn data_urls_split_into_type_and_data() {
        assert_eq!(
            parse_data_url("data:image/png;base64,AAAA"),
            Some(("image/png", "AAAA"))
        );
        assert_eq!(parse_data_url("https://example.org/a.png"), None);
        assert_eq!(parse_data_url("data:text/plain,hi"), None);
    }
}
