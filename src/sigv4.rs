//! Signature Version 4: what an S3 request carries to prove who sent it.
//!
//! The steps AWS documents, in order: a canonical form of the request, a
//! string to sign naming the moment and the scope, a signing key derived
//! from the secret through four HMACs, and the signature. Both sides are
//! here — a Location signs, [`crate::Session`] verifies — because verifying
//! is the same computation with a comparison at the end.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use transport::error::{Result, protocol_error};

use crate::percent::encode;
use crate::wire::Request;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SERVICE: &str = "s3";

/// The credential and region a signature is made under.
#[derive(Clone, Debug)]
pub struct Signer {
    region: String,
    access_key: String,
    secret_key: String,
}

impl Signer {
    #[must_use]
    pub fn new(region: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            region: region.to_string(),
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
        }
    }

    /// Sign `request` as of `at`, an `x-amz-date` such as [`now`] gives,
    /// adding `x-amz-date`, `x-amz-content-sha256` and `Authorization`.
    /// Every header already on the request is signed, so `Host` goes on
    /// first.
    #[must_use]
    pub fn sign(&self, request: Request, at: &str) -> Request {
        let payload = hex(&Sha256::digest(&request.body));
        let request = request
            .header("x-amz-date", at)
            .header("x-amz-content-sha256", &payload);
        let signed = signed_headers(&request.headers);
        let scope = self.scope(at);
        let signature = self.signature(at, &scope, &canonical(&request, &signed, &payload));
        let authorization = format!(
            "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
            self.access_key
        );
        request.header("Authorization", &authorization)
    }

    /// Whether `request` carries the signature this signer would have made.
    ///
    /// # Errors
    /// Where the request has no usable `Authorization`, names another
    /// credential, carries a payload hash its body does not match, or a
    /// signature that differs.
    pub fn verify(&self, request: &Request) -> Result<()> {
        let authorization = request
            .header_value("authorization")
            .ok_or_else(|| protocol_error("a request with no Authorization"))?;
        let (credential, signed, signature) = parts(authorization)?;
        let at = request
            .header_value("x-amz-date")
            .ok_or_else(|| protocol_error("a request with no x-amz-date"))?;
        if credential != format!("{}/{}", self.access_key, self.scope(at)) {
            return Err(protocol_error("a credential this signer does not hold"));
        }
        let payload = request
            .header_value("x-amz-content-sha256")
            .ok_or_else(|| protocol_error("a request with no x-amz-content-sha256"))?;
        if payload != hex(&Sha256::digest(&request.body)) {
            return Err(protocol_error("a payload hash the body does not match"));
        }
        let expected = self.signature(at, &self.scope(at), &canonical(request, signed, payload));
        if same(&expected, signature) {
            Ok(())
        } else {
            Err(protocol_error("a signature that does not match"))
        }
    }

    fn scope(&self, at: &str) -> String {
        format!("{}/{}/{SERVICE}/aws4_request", date_of(at), self.region)
    }

    fn signature(&self, at: &str, scope: &str, canonical: &str) -> String {
        let to_sign = format!(
            "{ALGORITHM}\n{at}\n{scope}\n{}",
            hex(&Sha256::digest(canonical.as_bytes()))
        );
        let key = [date_of(at), &self.region, SERVICE, "aws4_request"]
            .iter()
            .fold(
                format!("AWS4{}", self.secret_key).into_bytes(),
                |key, step| hmac(&key, step.as_bytes()),
            );
        hex(&hmac(&key, to_sign.as_bytes()))
    }
}

/// The canonical request: method, path, sorted query, the signed headers
/// with their values, the list of their names, and the payload hash.
#[must_use]
pub fn canonical(request: &Request, signed: &str, payload_hash: &str) -> String {
    let mut query: Vec<String> = request
        .query
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name, false), encode(value, false)))
        .collect();
    query.sort();
    let headers: Vec<String> = signed
        .split(';')
        .map(|name| {
            let value = request.header_value(name).unwrap_or("");
            format!(
                "{name}:{}\n",
                value.split_whitespace().collect::<Vec<_>>().join(" ")
            )
        })
        .collect();
    format!(
        "{}\n{}\n{}\n{}\n{signed}\n{payload_hash}",
        request.method,
        request.path,
        query.join("&"),
        headers.concat()
    )
}

/// The moment now, as `x-amz-date` writes it: `20260908T120000Z`.
#[must_use]
pub fn now() -> String {
    amz_date(SystemTime::now())
}

/// `at` as `x-amz-date` writes it.
#[must_use]
pub fn amz_date(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .unwrap_or(0);
    let (year, month, day) = civil(secs.div_euclid(86_400));
    let rest = secs.rem_euclid(86_400);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Year, month and day of a day count since 1970-01-01 — Howard Hinnant's
/// civil-from-days.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn date_of(at: &str) -> &str {
    at.get(..8).unwrap_or(at)
}

fn signed_headers(headers: &[(String, String)]) -> String {
    let mut names: Vec<String> = headers
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    names.sort();
    names.dedup();
    names.join(";")
}

fn parts(authorization: &str) -> Result<(&str, &str, &str)> {
    let rest = authorization
        .strip_prefix(ALGORITHM)
        .ok_or_else(|| protocol_error("an Authorization that is not Signature Version 4"))?;
    let mut credential = None;
    let mut signed = None;
    let mut signature = None;
    for part in rest.split(',') {
        match part.trim().split_once('=') {
            Some(("Credential", value)) => credential = Some(value),
            Some(("SignedHeaders", value)) => signed = Some(value),
            Some(("Signature", value)) => signature = Some(value),
            _ => {}
        }
    }
    match (credential, signed, signature) {
        (Some(credential), Some(signed), Some(signature)) => Ok((credential, signed, signature)),
        _ => Err(protocol_error(
            "an Authorization missing one of its three parts",
        )),
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        out
    })
}

/// Equal, without the comparison's timing saying how far the two agreed.
fn same(expected: &str, given: &str) -> bool {
    expected.len() == given.len()
        && expected
            .bytes()
            .zip(given.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// AWS's own worked example, "GET Object" in the Signature Version 4
    /// signing examples for S3.
    #[test]
    fn the_documented_example_signs_as_aws_says_it_does() {
        let signer = Signer::new(
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );
        let request = Request::new("GET", "/test.txt")
            .header("Host", "examplebucket.s3.amazonaws.com")
            .header("Range", "bytes=0-9");
        let sent = signer.sign(request, "20130524T000000Z");
        let authorization = sent.header_value("authorization").expect("signed");
        assert!(authorization.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
        assert!(authorization.ends_with(
            "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        ));
        signer.verify(&sent).expect("its own signature");
    }

    #[test]
    fn a_tampered_request_or_another_secret_does_not_verify() {
        let signer = Signer::new("eu-north-1", "AKID", "secret");
        let sent = signer.sign(
            Request::new("PUT", "/bucket/in/1.edi")
                .query("x", "1")
                .header("Host", "127.0.0.1:9000")
                .body(b"UNA"),
            &now(),
        );
        signer.verify(&sent).expect("verifies");
        let mut tampered = sent.clone();
        tampered.body = b"UNB".to_vec();
        assert!(signer.verify(&tampered).is_err());
        let mut tampered = sent.clone();
        tampered.query.push(("y".to_string(), "2".to_string()));
        assert!(signer.verify(&tampered).is_err());
        assert!(
            Signer::new("eu-north-1", "AKID", "other")
                .verify(&sent)
                .is_err()
        );
        assert!(
            Signer::new("eu-north-1", "OTHER", "secret")
                .verify(&sent)
                .is_err()
        );
        assert!(signer.verify(&Request::new("GET", "/")).is_err());
    }

    #[test]
    fn the_date_is_written_as_amazon_writes_it() {
        let at = UNIX_EPOCH + Duration::from_secs(1_369_353_600);
        assert_eq!(amz_date(at), "20130524T000000Z");
        assert_eq!(amz_date(UNIX_EPOCH), "19700101T000000Z");
        let at = UNIX_EPOCH + Duration::from_secs(951_782_400 + 3_661);
        assert_eq!(amz_date(at), "20000229T010101Z");
        assert!(now().ends_with('Z'));
    }
}
