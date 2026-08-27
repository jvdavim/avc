//! Just enough XML to read an S3 response.
//!
//! S3 replies are a fixed, shallow schema, so a scanner that finds elements by
//! name is sufficient and costs no dependency. It is deliberately not a parser:
//! it never validates structure and never interprets attributes.

/// Extract the text of every `<name>…</name>` element, in document order.
pub fn elements<'a>(document: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut found = Vec::new();
    let mut rest = document;
    while let Some(start) = rest.find(&open) {
        let body = &rest[start + open.len()..];
        let Some(end) = body.find(&close) else { break };
        found.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    found
}

/// Extract the text of the first `<name>…</name>` element.
pub fn element<'a>(document: &'a str, name: &str) -> Option<&'a str> {
    elements(document, name).into_iter().next()
}

/// Decode the five predefined XML entities.
///
/// S3 escapes keys and error messages with these and nothing else; numeric
/// character references do not appear in the responses we read.
pub fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult><IsTruncated>true</IsTruncated>
<Contents><Key>objects/sha256/ab/abcd</Key><Size>12</Size></Contents>
<Contents><Key>objects/sha256/cd/cdef</Key><Size>0</Size></Contents>
<NextContinuationToken>tok/en+1</NextContinuationToken></ListBucketResult>"#;

    #[test]
    fn reads_repeated_and_single_elements() {
        let contents = elements(LISTING, "Contents");
        assert_eq!(contents.len(), 2);
        assert_eq!(element(contents[0], "Key"), Some("objects/sha256/ab/abcd"));
        assert_eq!(element(contents[1], "Size"), Some("0"));
        assert_eq!(element(LISTING, "IsTruncated"), Some("true"));
        assert_eq!(element(LISTING, "NextContinuationToken"), Some("tok/en+1"));
        assert_eq!(element(LISTING, "Absent"), None);
    }

    #[test]
    fn decodes_entities_without_double_unescaping() {
        // `&amp;lt;` must survive as the literal text `&lt;`, not become `<`.
        assert_eq!(decode("a &amp;lt; b"), "a &lt; b");
        assert_eq!(decode("plain"), "plain");
    }

    #[test]
    fn unterminated_elements_do_not_loop() {
        assert!(elements("<Key>unclosed", "Key").is_empty());
    }
}
