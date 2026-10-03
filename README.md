# xmip-core-transport-s3

Amazon S3 transport: Signature Version 4 over the REST API — list a prefix, get each object and delete it once the runtime accepts it, put a Stream as an object — a bucket prefix is a Location. A technology of [xmip-core-transport](https://github.com/IlleNilsson/xmip-core-transport).

Both ends sign and verify with one signer (`client::signer`): the `s3`
scope, built `hashing_payload_in_header`, so the payload hash travels in
`x-amz-content-sha256` as S3 asks.

Signature Version 4 comes from [xmip-core-transport-aws](https://github.com/IlleNilsson/xmip-core-transport-aws), where every AWS technology shares what AWS speaks over HTTP (ADR-0044, amendment 2026-09-24); HTTP itself comes from [xmip-core-transport-http](https://github.com/IlleNilsson/xmip-core-transport-http).

Requests go on connections kept between them (`http::endpoint::Connections`, offering HTTP/1.1): the transport holds them and hands them to every client it makes, so a call costs one exchange and not a connect, a TLS handshake and a `Connection: close`, as it did until 2026-09-27.

## How a received object is acknowledged

A receive deletes nothing and gets nothing: it lists the prefix, and each object's `GET` is made when the runtime first reads its body (`transport::listed::listed`, the capability's one object-store receive, over `transport::body::fetched`), whole (`net::http` reads a response body whole), so a receive that lists a hundred objects holds none of them in memory. Every object stays in the bucket until the runtime gives its verdict after the whole receive cycle (runtime-model section 5). Accepted deletes the object. Refused deletes it too: a bucket has no place for a rejected object; the runtime has audited the refusal, and from Message creation on the Stream is kept in Xmip (ADR-0013). Failed leaves it, and the next receive lists and gets it again. Until 2026-10-02 a receive got every object it listed before handing any on. A crash before the verdict leaves it too: at-least-once, never a loss. The delete is the one the receive made until 2026-10-02, so a verdict adds no request.

A send target is read by `net::Target` in [xmip-core-library-net](https://github.com/IlleNilsson/xmip-core-library-net), the one reading of a URI every technology calls: scheme, authority, path and decoded query. Until 2026-09-28 it was read through the transport capability's `socket::target`, which split it on its first slash and left the query in the path.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
