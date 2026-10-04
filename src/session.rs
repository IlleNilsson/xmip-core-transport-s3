//! The far end: enough of S3 to answer one Location, and what a test or the
//! playground puts on loopback.
//!
//! Not S3. One session holds objects in memory, verifies every request
//! against one credential, and answers the four calls with the shapes S3
//! answers them — the listing, the object, the error with its code. A
//! Location that needs durability, versions or a second credential talks
//! to a store through [`crate::Client`].

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::time::Duration;

use transport::Taken;
use transport::error::Result;

use crate::client::signer;
use crate::xml;
use aws::sigv4::Signer;
use http::server;
use net::http::{Request, Response};
use net::percent::decode;

/// What the client did, as [`Session::serve_one`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The client listed `prefix` in `bucket`.
    Listed { bucket: String, prefix: String },
    /// The client fetched this object.
    Retrieved(String),
    /// The client stored an object; here is the Stream.
    Stored(Taken),
    /// The client deleted this object.
    Deleted(String),
    /// The client was answered with this S3 error code.
    Refused(String),
}

pub struct Session {
    signer: Signer,
    objects: BTreeMap<String, Vec<u8>>,
    /// Each object's `ETag`: the count of writes this session had made when
    /// it was written, so one written again is tagged anew.
    tags: BTreeMap<String, u64>,
    writes: u64,
    timeout: Option<Duration>,
}

impl Session {
    /// Answer requests signed in `region` as `access_key` with `secret_key`.
    #[must_use]
    pub fn new(region: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            signer: signer(region, access_key, secret_key),
            objects: BTreeMap::new(),
            tags: BTreeMap::new(),
            writes: 0,
            timeout: None,
        }
    }

    /// Give up on a client that stops mid-request after `timeout`.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Hold these objects, keyed `bucket/key`.
    #[must_use]
    pub fn with_objects(mut self, objects: BTreeMap<String, Vec<u8>>) -> Self {
        for (held, bytes) in objects {
            self.write(held, bytes);
        }
        self
    }

    /// What is held now, keyed `bucket/key`, stores and deletes included.
    #[must_use]
    pub fn objects(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.objects
    }

    /// Accept one connection on `listener`, answer its one request, and say
    /// what it was.
    ///
    /// # Errors
    /// Where the connection could not be accepted, broke, or sent nothing.
    pub fn serve_one(&mut self, listener: &TcpListener) -> Result<Event> {
        server::serve_one(listener, self.timeout, |request| self.answer(request))
    }

    fn answer(&mut self, request: &Request) -> (Event, Response) {
        if let Err(failure) = self.signer.verify(request) {
            return refused(403, "SignatureDoesNotMatch", &failure.message);
        }
        let path = decode(&request.path);
        let rest = path.trim_start_matches('/');
        let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return refused(400, "InvalidRequest", "a request naming no bucket");
        }
        match (request.method.as_str(), key.is_empty()) {
            ("GET", true) => self.list(bucket, request),
            ("GET", false) => self.get(bucket, key),
            ("PUT", false) => self.put(bucket, key, &request.body),
            ("DELETE", false) => self.delete(bucket, key),
            _ => refused(405, "MethodNotAllowed", "not one of the four calls"),
        }
    }

    fn list(&self, bucket: &str, request: &Request) -> (Event, Response) {
        let prefix = request
            .query_value("prefix")
            .unwrap_or_default()
            .to_string();
        let under = format!("{bucket}/{prefix}");
        let listed: Vec<(String, String)> = self
            .tags
            .iter()
            .filter(|(held, _)| held.starts_with(&under))
            .map(|(held, written)| (held[bucket.len() + 1..].to_string(), etag(*written)))
            .collect();
        let response = Response::new(200)
            .header("Content-Type", "application/xml")
            .body(xml::listing(bucket, &prefix, &listed).as_bytes());
        (
            Event::Listed {
                bucket: bucket.to_string(),
                prefix,
            },
            response,
        )
    }

    fn get(&self, bucket: &str, key: &str) -> (Event, Response) {
        match self.objects.get(&format!("{bucket}/{key}")) {
            Some(bytes) => (
                Event::Retrieved(origin(bucket, key)),
                Response::new(200)
                    .header("Content-Type", "application/octet-stream")
                    .body(bytes),
            ),
            None => refused(404, "NoSuchKey", "The specified key does not exist."),
        }
    }

    fn put(&mut self, bucket: &str, key: &str, bytes: &[u8]) -> (Event, Response) {
        let tag = self.write(format!("{bucket}/{key}"), bytes.to_vec());
        (
            Event::Stored(Taken::new(origin(bucket, key), bytes)),
            Response::new(200).header("ETag", &tag),
        )
    }

    /// Hold `bytes` as `held`, tagged anew, and say its `ETag`.
    fn write(&mut self, held: String, bytes: Vec<u8>) -> String {
        self.writes += 1;
        self.tags.insert(held.clone(), self.writes);
        self.objects.insert(held, bytes);
        etag(self.writes)
    }

    fn delete(&mut self, bucket: &str, key: &str) -> (Event, Response) {
        // S3 answers 204 whether or not the key was there.
        let held = format!("{bucket}/{key}");
        self.objects.remove(&held);
        self.tags.remove(&held);
        (Event::Deleted(origin(bucket, key)), Response::new(204))
    }
}

fn origin(bucket: &str, key: &str) -> String {
    format!("s3://{bucket}/{key}")
}

/// The `ETag` of the `written`th write, quoted as S3 quotes one.
fn etag(written: u64) -> String {
    format!("\"{written}\"")
}

fn refused(status: u16, code: &str, message: &str) -> (Event, Response) {
    (
        Event::Refused(code.to_string()),
        Response::new(status)
            .header("Content-Type", "application/xml")
            .body(xml::error(code, message).as_bytes()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "20260908T000000Z";

    fn signed(request: Request) -> Request {
        signer("r", "AKID", "secret").sign(request.header("Host", "s3.local"), AT)
    }

    #[test]
    fn a_session_answers_in_s3s_shapes_and_refuses_a_bad_signature() {
        let mut session = Session::new("r", "AKID", "secret");
        let (event, response) = session.answer(&signed(Request::new("PUT", "/b/k").body(b"x")));
        assert_eq!(response.status, 200);
        assert_eq!(event, Event::Stored(Taken::new("s3://b/k", b"x".to_vec())));
        assert_eq!(response.header_value("ETag"), Some("\"1\""));
        let (_, response) = session.answer(&signed(Request::new("PUT", "/b/k").body(b"x")));
        assert_eq!(
            response.header_value("ETag"),
            Some("\"2\""),
            "written again"
        );
        let (_, response) = session.answer(&signed(Request::new("GET", "/b").query("prefix", "k")));
        let listing = response.text().expect("text");
        let listed = xml::OBJECTS.objects(listing).expect("read");
        assert_eq!(listed, [("k".to_string(), "\"2\"".to_string())]);
        let (_, response) = session.answer(&signed(Request::new("GET", "/b").query("prefix", "z")));
        assert!(!response.text().expect("text").contains("<Key>"));
        let (event, response) = session.answer(&signed(Request::new("DELETE", "/b/k")));
        assert_eq!(
            (event, response.status),
            (Event::Deleted("s3://b/k".to_string()), 204)
        );
        assert!(session.objects().is_empty());
        let other = signer("r", "AKID", "wrong")
            .sign(Request::new("GET", "/b/k").header("Host", "s3.local"), AT);
        let (event, response) = session.answer(&other);
        assert_eq!(event, Event::Refused("SignatureDoesNotMatch".to_string()));
        assert_eq!(response.status, 403);
        let (_, response) = session.answer(&signed(Request::new("GET", "/")));
        assert_eq!(response.status, 400);
        let (_, response) = session.answer(&signed(Request::new("POST", "/b/k")));
        assert_eq!(response.status, 405);
    }
}
