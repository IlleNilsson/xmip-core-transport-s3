#![forbid(unsafe_code)]

//! Streams that arrive as objects in an S3 bucket. One object is one Stream,
//! its key kept beside it.
//!
//! S3 is the partner drop box of the cloud era, and every object store
//! since speaks its REST API: a bucket, keys under a prefix, four calls. A
//! Receive Location lists a prefix, gets each object and deletes it once it
//! is safely a Stream; a Send Location puts a Stream as an object. Both are
//! Signature Version 4 over plain HTTP/1.1 on a socket — `https://` with the
//! `tls` feature, which is the http technology's TLS (ADR-0033).
//!
//! ```text
//! sigv4.rs     signing a request, and verifying one
//! endpoint.rs  the endpoint, and a connection to it, TLS or plain
//! percent.rs   percent-encoding a key or a prefix
//! wire.rs      HTTP/1.1 on a socket, both sides
//! xml.rs       the listing and the error, picked by hand
//! client.rs    Xmip's side: list, get, put, delete
//! session.rs   the far end a test or the playground runs on loopback
//! ```
//!
//! S3 has objects and no lock this transport takes, so [`Transport::claims`]
//! answers [`NoNativeClaim`], ADR-0024 clause 5. The native claim the record
//! names — `PUT` with `If-None-Match: *` on a claim key — is a later step.
//!
//! The origin URI is the object in the protocol's own terms:
//! `s3://bucket/in/order-1.edi`. A send target is the same form, or a key
//! alone in this transport's bucket.

pub mod client;
pub mod endpoint;
pub mod percent;
pub mod session;
pub mod sigv4;
pub mod wire;
pub mod xml;

use std::time::Duration;

pub use client::Client;
pub use session::{Event, Session};
use transport::error::Result;
use transport::socket;
use transport::{Arrived, Directions, NoNativeClaim, ResourceClaim, Transport};

pub struct S3Transport {
    endpoint: String,
    region: String,
    bucket: String,
    access_key: String,
    secret_key: String,
    prefix: String,
    timeout: Option<Duration>,
}

impl S3Transport {
    /// Speak to the endpoint at `endpoint` — `http://host:port` or
    /// `https://host:port` — in `region`, about `bucket`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, region: &str, bucket: &str) -> Self {
        Self {
            endpoint: endpoint.into(),
            region: region.to_string(),
            bucket: bucket.to_string(),
            access_key: String::new(),
            secret_key: String::new(),
            prefix: String::new(),
            timeout: None,
        }
    }

    /// Sign as this access key.
    #[must_use]
    pub fn with_credentials(mut self, access_key: &str, secret_key: &str) -> Self {
        self.access_key = access_key.to_string();
        self.secret_key = secret_key.to_string();
        self
    }

    /// Receive only what is under `prefix` — `in/`, say.
    #[must_use]
    pub fn with_prefix(mut self, prefix: &str) -> Self {
        self.prefix = prefix.to_string();
        self
    }

    /// Give up on an endpoint that stops answering after `timeout`.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The client this transport speaks through.
    ///
    /// # Errors
    /// Where the endpoint is not an HTTP URL.
    pub fn client(&self) -> Result<Client> {
        let client = Client::new(
            &self.endpoint,
            &self.region,
            &self.access_key,
            &self.secret_key,
        )?;
        Ok(match self.timeout {
            Some(timeout) => client.timing_out_after(timeout),
            None => client,
        })
    }

    /// A far end that holds this transport's credentials, for a test or the
    /// playground to run on loopback.
    #[must_use]
    pub fn session(&self) -> Session {
        let session = Session::new(&self.region, &self.access_key, &self.secret_key);
        match self.timeout {
            Some(timeout) => session.timing_out_after(timeout),
            None => session,
        }
    }

    /// Where a target names the bucket and key itself — `s3://bucket/key`
    /// — or is a key alone in this transport's bucket.
    fn resolve<'a>(&'a self, target: &'a str) -> (&'a str, &'a str) {
        socket::target("s3", target).unwrap_or((&self.bucket, target))
    }
}

impl Transport for S3Transport {
    fn name(&self) -> &'static str {
        "s3"
    }

    fn directions(&self) -> Directions {
        Directions::BOTH
    }

    /// Every object under the prefix, each deleted once it is a Stream.
    fn receive(&self) -> Result<Vec<Arrived>> {
        let client = self.client()?;
        let mut arrived = Vec::new();
        for key in client.list(&self.bucket, &self.prefix)? {
            let bytes = client.get(&self.bucket, &key)?;
            client.delete(&self.bucket, &key)?;
            arrived.push(Arrived::new(format!("s3://{}/{key}", self.bucket), bytes));
        }
        Ok(arrived)
    }

    fn send(&self, target: &str, bytes: &[u8]) -> Result<()> {
        let (bucket, key) = self.resolve(target);
        self.client()?.put(bucket, key, bytes)
    }

    fn claims(&self) -> Option<&dyn ResourceClaim> {
        Some(&NoNativeClaim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn node(endpoint: &str, secret: &str) -> S3Transport {
        S3Transport::new(endpoint, "eu-north-1", "orders")
            .with_credentials("AKID", secret)
            .with_prefix("in/")
            .timing_out_after(Duration::from_secs(2))
    }

    fn serve(
        mut session: Session,
        listener: TcpListener,
        requests: usize,
    ) -> JoinHandle<(Session, Vec<Event>)> {
        std::thread::spawn(move || {
            let events = (0..requests)
                .map(|_| session.serve_one(&listener).expect("served"))
                .collect();
            (session, events)
        })
    }

    #[test]
    fn what_is_sent_to_a_session_is_received_back_and_deleted() {
        let (listener, address) = socket::bind_tcp("127.0.0.1:0").expect("bind");
        let near = node(&format!("http://{address}"), "secret");
        // Two puts, one list, then a get and a delete per object under in/.
        let far_end = serve(near.session(), listener, 7);
        near.send("in/1.edi", b"UNA:+.? '").expect("a key alone");
        near.send("s3://orders/in/2.edi", b"")
            .expect("a full target");
        let mut arrived = near.receive().expect("received");
        arrived.sort_by(|a, b| a.origin_uri.cmp(&b.origin_uri));
        assert_eq!(arrived.len(), 2);
        assert_eq!(arrived[0].origin_uri, "s3://orders/in/1.edi");
        assert_eq!(arrived[0].bytes, b"UNA:+.? '");
        assert_eq!(arrived[1].origin_uri, "s3://orders/in/2.edi");
        assert!(arrived[1].bytes.is_empty());
        let (session, events) = far_end.join().expect("thread");
        assert!(session.objects().is_empty(), "deleted after retrieve");
        assert_eq!(
            events[0],
            Event::Stored(Arrived::new("s3://orders/in/1.edi", b"UNA:+.? '".to_vec()))
        );
        let deleted = events.iter().filter(|e| matches!(e, Event::Deleted(_)));
        assert_eq!(deleted.count(), 2);
    }

    #[test]
    fn a_wrong_secret_is_refused_with_s3s_own_status_and_code() {
        let (listener, address) = socket::bind_tcp("127.0.0.1:0").expect("bind");
        let far_end = serve(node("http://x", "secret").session(), listener, 1);
        let failure = node(&format!("http://{address}"), "wrong")
            .send("in/1.edi", b"x")
            .expect_err("refused");
        assert!(
            failure.message.contains("403 SignatureDoesNotMatch"),
            "{failure}"
        );
        assert!(!failure.retryable);
        let (_, events) = far_end.join().expect("thread");
        assert_eq!(
            events,
            vec![Event::Refused("SignatureDoesNotMatch".to_string())]
        );
    }

    #[test]
    fn objects_are_artefacts_without_a_lock_and_an_unreachable_endpoint_is_retryable() {
        let near = node("http://127.0.0.1:1", "secret");
        assert!(near.claims().is_some());
        assert_eq!(near.name(), "s3");
        assert!(near.directions().receives() && near.directions().sends());
        assert!(near.receive().expect_err("nothing listening").retryable);
        let failure = node("orders.local", "s")
            .send("k", b"")
            .expect_err("no scheme");
        assert!(!failure.retryable);
    }
}
