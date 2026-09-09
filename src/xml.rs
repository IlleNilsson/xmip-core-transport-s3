//! The little XML S3 speaks: a listing naming keys, an error naming a code.
//!
//! Written by hand, both documents flat; read back by the capability's
//! flat scan (ADR-0044), which is a scan, not a tree, because the one
//! question either side asks is the text of every element by one name.

use transport::xml::escape;
pub use transport::xml::{first, texts};

/// A `ListBucketResult` naming `keys` under `prefix` in `bucket`.
#[must_use]
pub fn listing(bucket: &str, prefix: &str, keys: &[String]) -> String {
    let contents: Vec<String> = keys
        .iter()
        .map(|key| format!("<Contents><Key>{}</Key></Contents>", escape(key)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>{}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount>\
         <MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated>{}\
         </ListBucketResult>",
        escape(bucket),
        escape(prefix),
        keys.len(),
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

    #[test]
    fn a_listing_names_its_keys_back_with_entities_intact() {
        let keys = vec!["in/a&b.edi".to_string(), "in/<c>.edi".to_string()];
        let xml = listing("orders", "in/", &keys);
        assert!(xml.contains("<Key>in/a&amp;b.edi</Key>"));
        assert_eq!(texts(&xml, "Key"), keys);
        assert_eq!(first(&xml, "Prefix").as_deref(), Some("in/"));
        assert!(texts(&xml, "Absent").is_empty());
        assert!(texts("<Key>unclosed", "Key").is_empty());
    }

    #[test]
    fn an_error_names_its_code() {
        let xml = error(
            "SignatureDoesNotMatch",
            "The request signature we calculated",
        );
        assert_eq!(
            first(&xml, "Code").as_deref(),
            Some("SignatureDoesNotMatch")
        );
        assert_eq!(first(&xml, "Absent"), None);
    }
}
