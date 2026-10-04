//! The little XML S3 speaks: a listing naming keys with their `ETag`s, an
//! error naming a code.
//!
//! Written by hand, both documents flat; read back by the
//! estate's flat scan (`codec::xml`), which is a scan, not a tree, because the one
//! question either side asks is the text of an element by its name.

use codec::xml::escape;
use transport::listed::Listing;

/// How a `ListBucketResult` names an object: each `<Contents>`' key with
/// its `ETag`, the stamp that changes whenever it is written again; read
/// by the capability's one listing scan.
pub const OBJECTS: Listing = Listing {
    entry: "Contents",
    name: "Key",
    stamp: "ETag",
};

/// A `ListBucketResult` naming `objects` — each a key with its `ETag` —
/// under `prefix` in `bucket`.
#[must_use]
pub fn listing(bucket: &str, prefix: &str, objects: &[(String, String)]) -> String {
    let contents: Vec<String> = objects
        .iter()
        .map(|(key, tag)| {
            format!(
                "<Contents><Key>{}</Key><ETag>{}</ETag></Contents>",
                escape(key),
                escape(tag)
            )
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>{}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount>\
         <MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>{}\
         </ListBucketResult>",
        escape(bucket),
        escape(prefix),
        objects.len(),
        contents.concat()
    )
}

/// An `Error` naming `code`.
#[must_use]
pub fn error(code: &str, message: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <Error><Code>{}</Code><Message>{}</Message></Error>",
        escape(code),
        escape(message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec::xml::{text, texts};

    #[test]
    fn a_listing_names_its_keys_and_etags_back_with_entities_intact() {
        let held = vec![
            ("in/a&b.edi".to_string(), "\"1\"".to_string()),
            ("in/<c>.edi".to_string(), "\"2\"".to_string()),
        ];
        let xml = listing("orders", "in/", &held);
        assert!(xml.contains("<Key>in/a&amp;b.edi</Key><ETag>&quot;1&quot;</ETag>"));
        assert_eq!(OBJECTS.objects(&xml).expect("read"), held);
        assert_eq!(text(&xml, "Prefix").expect("read").as_deref(), Some("in/"));
        assert!(texts(&xml, "Absent").expect("read").is_empty());
        assert!(
            OBJECTS
                .objects("<Contents><Key>unclosed")
                .expect("read")
                .is_empty()
        );
        let untagged = OBJECTS
            .objects("<Contents><Key>k</Key></Contents>")
            .expect_err("no ETag");
        assert!(untagged.message.contains("ETag"), "{untagged}");
    }

    #[test]
    fn an_error_names_its_code() {
        let xml = error(
            "SignatureDoesNotMatch",
            "The request signature we calculated",
        );
        assert_eq!(
            text(&xml, "Code").expect("read").as_deref(),
            Some("SignatureDoesNotMatch")
        );
        assert_eq!(text(&xml, "Absent").expect("read"), None);
    }
}
