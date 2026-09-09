//! Xmip's side: the four calls a Location makes, each one signed request
//! over one connection.
//!
//! Path-style addressing — `/bucket/key` at one endpoint — because it is
//! what every S3-compatible store speaks and what a private endpoint
//! without wildcard DNS can serve; virtual-hosted style is the same request
//! with the bucket moved into the host.

use std::time::Duration;

use transport::error::{Result, TransportError};

use crate::sigv4::{self, Signer};
use crate::xml;
use http::endpoint;
use http::message::{self, Request, Response};
use http::percent::encode;

pub struct Client {
    endpoint: String,
    host: String,
    signer: Signer,
    timeout: Option<Duration>,
}

impl Client {
    /// Speak to the S3 endpoint at `endpoint` — `http://host:port` or
    /// `https://host:port` — in `region`, as `access_key`.
    ///
    /// # Errors
    /// Where `endpoint` is not an HTTP URL.
    pub fn new(endpoint: &str, region: &str, access_key: &str, secret_key: &str) -> Result<Self> {
        Ok(Self {
            endpoint: endpoint.to_string(),
            host: endpoint::authority(endpoint)?,
            signer: Signer::new(region, access_key, secret_key),
            timeout: None,
        })
    }

    /// Give up on an endpoint that stops answering after `timeout`.
    #[must_use]
    pub const fn timing_out_after(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The keys under `prefix` in `bucket`, as many as one listing carries
    /// — a thousand — so a fuller prefix is taken a thousand at a time.
    ///
    /// # Errors
    /// Where the endpoint refused, could not be reached, or did not answer
    /// with a listing.
    pub fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<String>> {
        let request = Request::new("GET", format!("/{}", encode(bucket, false)))
            .query("list-type", "2")
            .query("prefix", prefix);
        Ok(xml::texts(&self.call(request)?.text(), "Key"))
    }

    /// The object at `key` in `bucket`.
    ///
    /// # Errors
    /// Where there is no such object, or the endpoint refused or could not
    /// be reached.
    pub fn get(&self, bucket: &str, key: &str) -> Result<Vec<u8>> {
        Ok(self.call(Request::new("GET", object(bucket, key)))?.body)
    }

    /// Store `bytes` as the object at `key` in `bucket`.
    ///
    /// # Errors
    /// Where the endpoint refused or could not be reached.
    pub fn put(&self, bucket: &str, key: &str, bytes: &[u8]) -> Result<()> {
        let request = Request::new("PUT", object(bucket, key))
            .header("Content-Type", "application/octet-stream")
            .body(bytes);
        self.call(request).map(|_| ())
    }

    /// Delete the object at `key` in `bucket`.
    ///
    /// # Errors
    /// Where the endpoint refused or could not be reached.
    pub fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        self.call(Request::new("DELETE", object(bucket, key)))
            .map(|_| ())
    }

    fn call(&self, request: Request) -> Result<Response> {
        let signed = self
            .signer
            .sign(request.header("Host", &self.host), &sigv4::now());
        let stream = endpoint::connect(&self.endpoint, self.timeout)?;
        judge(message::exchange(stream, &signed)?)
    }
}

fn object(bucket: &str, key: &str) -> String {
    format!("/{}/{}", encode(bucket, false), encode(key, true))
}

/// A 2xx answer as it is; anything else as a failure naming the status and
/// the code S3 put in the body, retryable where S3 says come back.
fn judge(response: Response) -> Result<Response> {
    if (200..300).contains(&response.status) {
        return Ok(response);
    }
    let code = xml::first(&response.text(), "Code").unwrap_or_default();
    let retryable = response.status >= 500
        || response.status == 408
        || response.status == 429
        || code == "SlowDown";
    Err(TransportError {
        message: format!("S3 answered {} {code}", response.status),
        retryable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Event, Session};
    use transport::socket;

    #[test]
    fn the_four_calls_reach_a_session_and_come_back_shaped_as_s3_shapes_them() {
        let (listener, address) = socket::bind_tcp("127.0.0.1:0").expect("bind");
        let far_end = std::thread::spawn(move || {
            let mut session = Session::new("eu-north-1", "AKID", "secret")
                .timing_out_after(Duration::from_secs(2));
            let events: Vec<Event> = (0..6)
                .map(|_| session.serve_one(&listener).expect("served"))
                .collect();
            (session, events)
        });
        let client = Client::new(&format!("http://{address}"), "eu-north-1", "AKID", "secret")
            .expect("endpoint")
            .timing_out_after(Duration::from_secs(2));
        client.put("orders", "in/a b.edi", b"UNA").expect("put");
        client.put("orders", "out/c.edi", b"UNB").expect("put");
        assert_eq!(
            client.list("orders", "in/").expect("list"),
            vec!["in/a b.edi".to_string()]
        );
        assert_eq!(client.get("orders", "in/a b.edi").expect("get"), b"UNA");
        client.delete("orders", "in/a b.edi").expect("delete");
        let missing = client.get("orders", "in/a b.edi").expect_err("gone");
        assert!(missing.message.contains("404 NoSuchKey"), "{missing}");
        assert!(!missing.retryable);
        let (session, events) = far_end.join().expect("thread");
        assert_eq!(session.objects().len(), 1);
        assert!(matches!(&events[2], Event::Listed { prefix, .. } if prefix == "in/"));
        assert_eq!(
            events[3],
            Event::Retrieved("s3://orders/in/a b.edi".to_string())
        );
        assert_eq!(
            events[4],
            Event::Deleted("s3://orders/in/a b.edi".to_string())
        );
        assert_eq!(events[5], Event::Refused("NoSuchKey".to_string()));
    }

    #[test]
    fn a_server_failure_is_worth_repeating_and_a_client_one_is_not() {
        assert!(judge(Response::new(503)).expect_err("server").retryable);
        assert!(judge(Response::new(429)).expect_err("throttled").retryable);
        let slow = Response::new(400).body(xml::error("SlowDown", "").as_bytes());
        assert!(judge(slow).expect_err("slow down").retryable);
        assert!(!judge(Response::new(403)).expect_err("forbidden").retryable);
        assert!(Client::new("orders.local", "r", "a", "s").is_err());
    }
}
