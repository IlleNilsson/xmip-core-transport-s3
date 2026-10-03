#![forbid(unsafe_code)]

//! Streams that arrive as objects in an S3 bucket. One object is one Stream,
//! its key kept beside it.
//!
//! S3 is the Party drop box of the cloud era, and every object store
//! since speaks its REST API: a bucket, keys under a prefix, four calls. A
//! Receive Location lists a prefix, gets each object as the runtime first
//! reads it, and deletes it once the runtime accepts or refuses it after
//! the whole receive cycle — one whose cycle failed stays for the next
//! receive; a Send Location puts a Stream as an object.
//! Both are Signature Version 4 over plain HTTP/1.1 on a socket —
//! `https://` with the `tls` feature, which is the http technology's TLS
//! (ADR-0033).
//!
//! ```text
//! xml.rs       the listing and the error, picked by hand
//! client.rs    Xmip's side: list, get, put, delete
//! session.rs   the far end a test or the playground runs on loopback
//! ```
//!
//! The endpoint, HTTP itself and the judgement of an answer come from the
//! http technology, the percent-encoding from `net`, Signature Version 4
//! from the AWS crate, the flat XML scan from the capability (ADR-0044). The signer
//! lived here until 2026-09-14, when aws-sqs was found importing it, and in
//! the http technology until the owner's ruling of 2026-09-22: what AWS
//! speaks is the AWS crate's to share.
//!
//! S3 has objects and no lock this transport takes, so [`Transport::claims`]
//! answers [`NoNativeClaim`], ADR-0024 clause 5. The native claim the record
//! names — `PUT` with `If-None-Match: *` on a claim key — is a later step.
//!
//! The origin URI is the object in the protocol's own terms:
//! `s3://bucket/in/order-1.edi`. A send target is the same form, or a key
//! alone in this transport's bucket.

pub mod client;
pub mod session;
pub mod xml;

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

pub use client::Client;
use http::endpoint::Connections;
use net::{Endpoint, Target};
pub use session::{Event, Session};
use transport::error::{Result, protocol_error};
use transport::listed::listed;
use transport::listening::Listening;
use transport::loopback::{FarEnd, LOOPBACK_TIMEOUT, Loopback};
use transport::socket;
use transport::{Arrived, Configured, Directions, NoNativeClaim, ResourceClaim, Transport};
use xcore::settings::{Applies, Kind, Presence, Read, Setting, Settings};

/// What the loopback pair agrees on: one bucket, one object put there, one
/// credential in one region that the far end expects and the near end
/// signs as.
const LOOPBACK_BUCKET: &str = "probe";
const LOOPBACK_OBJECT: &str = "probe.bin";
const LOOPBACK_REGION: &str = "eu-north-1";
const LOOPBACK_ACCESS_KEY: &str = "AKIDPROBE";
const LOOPBACK_SECRET_KEY: &str = "probe";

#[derive(Clone)]
pub struct S3Transport {
    endpoint: String,
    region: String,
    bucket: String,
    access_key: String,
    secret_key: String,
    prefix: String,
    timeout: Option<Duration>,
    /// The connections kept to the service, shared by every client this
    /// makes.
    connections: Connections,
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
            connections: Connections::new(),
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
        let client = client.sharing(self.connections.clone());
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
        Target::under(&["s3"], target).map_or((&self.bucket, target), |named| {
            (named.authority(), named.path())
        })
    }
}

impl Transport for S3Transport {
    fn name(&self) -> &'static str {
        "s3"
    }

    fn directions(&self) -> Directions {
        Directions::BOTH
    }

    fn arrivals(&self) -> transport::Arrivals {
        transport::Arrivals::Ordered("a receive lists again what is not yet told")
    }

    /// Every object under the prefix, listed by [`listed`], the capability's
    /// one object-store receive: the receive gets and deletes nothing, each
    /// object's `GET` is made when the runtime first reads its body — whole,
    /// `net::http` reads a `GET` body whole — and its acknowledgement
    /// deletes it on [`transport::Verdict::Accepted`] and
    /// [`transport::Verdict::Refused`] (a bucket has no place for a rejected
    /// object) and leaves it on [`transport::Verdict::Failed`], for the next
    /// receive to list and get again.
    fn receive(&self) -> Result<Vec<Arrived>> {
        let client = Arc::new(self.client()?);
        let (getting, deleting) = (Arc::clone(&client), Arc::clone(&client));
        let (bucket, getting_from, deleting_from) = (
            self.bucket.as_str(),
            self.bucket.clone(),
            self.bucket.clone(),
        );
        listed(
            || client.list(bucket, &self.prefix),
            |key| format!("s3://{bucket}/{key}"),
            move |key| getting.get(&getting_from, key),
            move |key| deleting.delete(&deleting_from, key),
        )
    }

    fn send(&self, target: &str, bytes: &[u8]) -> Result<()> {
        let (bucket, key) = self.resolve(target);
        self.client()?.put(bucket, key, bytes)
    }

    fn claims(&self) -> Option<&dyn ResourceClaim> {
        Some(&NoNativeClaim)
    }
}

impl Configured for S3Transport {
    /// The address is the S3 endpoint, `https://s3.<region>.amazonaws.com`.
    /// The access key and its secret are the Location's credentials, not
    /// settings: a secret never is.
    const SETTINGS: &'static Settings = &Settings {
        technology: env!("CARGO_PKG_NAME"),
        settings: &[
            Setting {
                name: "region",
                kind: Kind::Text,
                presence: Presence::Required,
                meaning: "The AWS region requests are signed for, eu-north-1.",
                applies: Applies::Both,
            },
            Setting {
                name: "bucket",
                kind: Kind::Text,
                presence: Presence::Required,
                meaning: "The bucket objects are taken from, and put in when a send target \
                          names none.",
                applies: Applies::Both,
            },
            Setting {
                name: "prefix",
                kind: Kind::Text,
                presence: Presence::Optional,
                meaning: "The key prefix a Receive Location takes objects under, in/; the \
                          whole bucket when left out.",
                applies: Applies::Receive,
            },
            Setting {
                name: "timeout",
                kind: Kind::Duration,
                presence: Presence::Optional,
                meaning: "How long an endpoint that stops answering is waited on; unbounded \
                          when left out.",
                applies: Applies::Both,
            },
        ],
    };

    fn configured(address: &str, settings: &Read) -> Result<Self> {
        // The access key and secret come through the Location's credentials.
        let mut transport = Self::new(address, settings.text("region"), settings.text("bucket"));
        if let Some(prefix) = settings.optional_text("prefix") {
            transport = transport.with_prefix(prefix);
        }
        Ok(match settings.optional_duration("timeout") {
            Some(timeout) => transport.timing_out_after(timeout),
            None => transport,
        })
    }
}

impl S3Transport {
    /// Both ends on this machine: an ephemeral local port, one credential
    /// the far end expects and the near end signs as, the loopback timeout.
    #[must_use]
    pub fn loopback() -> Self {
        Self::new("http://127.0.0.1:0", LOOPBACK_REGION, LOOPBACK_BUCKET)
            .with_credentials(LOOPBACK_ACCESS_KEY, LOOPBACK_SECRET_KEY)
            .timing_out_after(LOOPBACK_TIMEOUT)
    }
}

impl Loopback for S3Transport {
    /// A bound session waiting for its one store. S3 opens a connection per
    /// call, so the session serves one request at a time until one stored.
    fn far_end(&self) -> Result<Box<dyn FarEnd>> {
        let mut session = self.session();
        Ok(Box::new(Listening::new(
            move |listener: &TcpListener| loop {
                match session.serve_one(listener)? {
                    Event::Stored(arrived) => return Ok(arrived),
                    Event::Refused(code) => {
                        return Err(protocol_error(format!("the session refused: {code}")));
                    }
                    _ => {}
                }
            },
            socket::bind_tcp(&Endpoint::parse(&self.endpoint)?.address())?,
        )))
    }

    /// Put the payload as one object, from a fresh near end signing as
    /// this transport does, at the endpoint on `address`.
    fn send_to(&self, address: &str, payload: &[u8]) -> Result<()> {
        let near = Self {
            endpoint: format!("http://{address}"),
            ..self.clone()
        };
        near.send(LOOPBACK_OBJECT, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::JoinHandle;
    use transport::Taken;

    fn node(endpoint: &str, secret: &str) -> S3Transport {
        S3Transport::new(endpoint, "eu-north-1", "orders")
            .with_credentials("AKID", secret)
            .with_prefix("in/")
            .timing_out_after(Duration::from_secs(2))
    }

    #[test]
    fn s3_declares_its_settings_and_reads_through_them() {
        use xcore::settings::Given;
        assert_eq!(S3Transport::SETTINGS.problems(), Vec::<String>::new());
        let text = |name: &str, value: &str| (name.to_string(), Given::Text(value.to_string()));
        let endpoint = "https://s3.eu-north-1.amazonaws.com";
        let given = [
            text("region", "eu-north-1"),
            text("bucket", "orders"),
            text("prefix", "in/"),
            text("timeout", "5s"),
        ];
        let received = S3Transport::open(endpoint, Applies::Receive, &given).expect("built");
        assert_eq!(received.endpoint, endpoint);
        assert_eq!(
            (received.region.as_str(), received.bucket.as_str()),
            ("eu-north-1", "orders")
        );
        assert_eq!(received.prefix, "in/");
        assert_eq!(received.timeout, Some(Duration::from_secs(5)));
        assert!(
            received.secret_key.is_empty(),
            "the secret is the credentials'"
        );
        let Err(refused) = S3Transport::open(endpoint, Applies::Send, &given[..1]) else {
            panic!("the bucket is required");
        };
        assert!(refused.message.contains("\"bucket\""), "{refused}");
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
    fn an_object_is_got_when_read_and_deleted_when_accepted_or_refused_and_kept_when_failed() {
        let (listener, address) = socket::bind_tcp("127.0.0.1:0").expect("bind");
        let near = node(&format!("http://{address}"), "secret");
        // Three puts; a list, the one get read, two deletes; a list, a get,
        // a delete.
        let far_end = serve(near.session(), listener, 10);
        near.send("in/1.edi", b"UNA:+.? '").expect("a key alone");
        near.send("s3://orders/in/2.edi", b"")
            .expect("a full target");
        near.send("in/3.edi", b"C3").expect("a key alone");
        let mut arrived = near.receive().expect("received");
        arrived.sort_by(|a, b| a.origin_uri.cmp(&b.origin_uri));
        assert_eq!(arrived.len(), 3);
        assert!(arrived.iter().all(Arrived::defers));
        let third = arrived.pop().expect("third");
        let second = arrived.pop().expect("second");
        assert_eq!(second.origin_uri, "s3://orders/in/2.edi");
        let first = arrived.pop().expect("first").taken().expect("accepted");
        assert_eq!(first.origin_uri, "s3://orders/in/1.edi");
        assert_eq!(first.bytes, b"UNA:+.? '");
        second.failed().expect("left");
        third
            .refused(transport::Refusal::Unacceptable)
            .expect("deleted");
        let again = near.receive().expect("received again");
        assert_eq!(again.len(), 1, "the failed one, and only it");
        let again = again.into_iter().next().expect("one").taken().expect("ok");
        assert_eq!(again.origin_uri, "s3://orders/in/2.edi");
        assert!(again.bytes.is_empty());
        let (session, events) = far_end.join().expect("thread");
        assert!(session.objects().is_empty(), "deleted once answered");
        assert_eq!(
            events[0],
            Event::Stored(Taken::new("s3://orders/in/1.edi", b"UNA:+.? '".to_vec()))
        );
        let named = |key: &str| format!("s3://orders/in/{key}");
        assert!(matches!(events[3], Event::Listed { .. }));
        assert_eq!(
            events[4..7],
            [
                Event::Retrieved(named("1.edi")),
                Event::Deleted(named("1.edi")),
                Event::Deleted(named("3.edi")),
            ],
            "only what was read was got"
        );
        assert!(matches!(events[7], Event::Listed { .. }));
        assert_eq!(
            events[8..],
            [
                Event::Retrieved(named("2.edi")),
                Event::Deleted(named("2.edi"))
            ]
        );
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

    #[test]
    fn the_loopback_stores_one_object_through_its_own_session() {
        let pair = S3Transport::loopback();
        let arrived = pair.round(b"an object").expect("round");
        assert_eq!(arrived.bytes, b"an object");
        assert_eq!(arrived.origin_uri, "s3://probe/probe.bin");
        assert_eq!(pair.name(), "s3");
        assert_eq!(pair.ceiling(), None);
    }

    /// The Playground's edge payloads, written here so the crate does not
    /// depend on it.
    fn edge_payloads() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("empty", Vec::new()),
            ("one byte", vec![0x2a]),
            ("every byte", (0..=255).collect()),
            ("nul run", vec![0; 512]),
            ("high bytes", vec![0xff; 512]),
            ("crlf storm", b"\r\n".repeat(400)),
        ]
    }

    #[test]
    fn the_loopback_returns_the_edge_payloads_whole() {
        let pair = S3Transport::loopback();
        for (name, payload) in edge_payloads() {
            assert!(pair.refuses(&payload).is_none(), "{name}");
            let arrived = pair.round(&payload).expect(name);
            assert_eq!(arrived.bytes, payload, "{name}");
        }
    }
}
