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

use transport::Arrived;
use transport::error::{Result, protocol_error};
use transport::socket;

use crate::percent::decode;
use crate::sigv4::Signer;
use crate::wire::{self, Request, Response};
use crate::xml;

/// What the client did, as [`Session::serve_one`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The client listed `prefix` in `bucket`.
    Listed { bucket: String, prefix: String },
    /// The client fetched this object.
    Retrieved(String),
    /// The client stored an object; here is the Stream.
    Stored(Arrived),
    /// The client deleted this object.
    Deleted(String),
    /// The client was answered with this S3 error code.
    Refused(String),
}

pub struct Session {
    signer: Signer,
    objects: BTreeMap<String, Vec<u8>>,
    timeout: Option<Duration>,
}

impl Session {
    /// Answer requests signed in `region` as `access_key` with `secret_key`.
    #[must_use]
    pub fn new(region: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            signer: Signer::new(region, access_key, secret_key),
            objects: BTreeMap::new(),
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
        self.objects = objects;
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
        let (stream, _) = socket::accept_tcp(listener, self.timeout)?;
        let (mut reader, mut writer) = socket::split(stream)?;
        let request = wire::read_request(&mut reader)?
            .ok_or_else(|| protocol_error("a connection that sent no request"))?;
        let (event, response) = self.answer(&request);
        wire::write_response(&mut writer, &response)?;
        Ok(event)
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
        let keys: Vec<String> = self
            .objects
            .keys()
            .filter(|held| held.starts_with(&under))
            .map(|held| held[bucket.len() + 1..].to_string())
            .collect();
        let response = Response::new(200)
            .header("Content-Type", "application/xml")
            .body(xml::listing(bucket, &prefix, &keys).as_bytes());
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
        self.objects
            .insert(format!("{bucket}/{key}"), bytes.to_vec());
        (
            Event::Stored(Arrived::new(origin(bucket, key), bytes)),
            Response::new(200).header("ETag", "\"xmip\""),
        )
    }

    fn delete(&mut self, bucket: &str, key: &str) -> (Event, Response) {
        // S3 answers 204 whether or not the key was there.
        self.objects.remove(&format!("{bucket}/{key}"));
        (Event::Deleted(origin(bucket, key)), Response::new(204))
    }
}

fn origin(bucket: &str, key: &str) -> String {
    format!("s3://{bucket}/{key}")
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
        Signer::new("r", "AKID", "secret").sign(request.header("Host", "s3.local"), AT)
    }

    #[test]
    fn a_session_answers_in_s3s_shapes_and_refuses_a_bad_signature() {
        let mut session = Session::new("r", "AKID", "secret");
        let (event, response) = session.answer(&signed(Request::new("PUT", "/b/k").body(b"x")));
        assert_eq!(response.status, 200);
        assert_eq!(
            event,
            Event::Stored(Arrived::new("s3://b/k", b"x".to_vec()))
        );
        let (_, response) = session.answer(&signed(Request::new("GET", "/b").query("prefix", "k")));
        assert!(response.text().contains("<Key>k</Key>"));
        let (_, response) = session.answer(&signed(Request::new("GET", "/b").query("prefix", "z")));
        assert!(!response.text().contains("<Key>"));
        let (event, response) = session.answer(&signed(Request::new("DELETE", "/b/k")));
        assert_eq!(
            (event, response.status),
            (Event::Deleted("s3://b/k".to_string()), 204)
        );
        assert!(session.objects().is_empty());
        let other = Signer::new("r", "AKID", "wrong")
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
