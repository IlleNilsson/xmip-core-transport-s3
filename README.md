# xmip-core-transport-s3

Amazon S3 transport: Signature Version 4 over the REST API — list a prefix, get and delete each object, put a Stream as an object — a bucket prefix is a Location. A technology of [xmip-core-transport](https://github.com/IlleNilsson/xmip-core-transport).

Both ends sign and verify with one signer (`client::signer`): the `s3`
scope, built `hashing_payload_in_header`, so the payload hash travels in
`x-amz-content-sha256` as S3 asks.

Signature Version 4 comes from [xmip-core-transport-aws](https://github.com/IlleNilsson/xmip-core-transport-aws), where every AWS technology shares what AWS speaks over HTTP (ADR-0044, amendment 2026-09-24); HTTP itself comes from [xmip-core-transport-http](https://github.com/IlleNilsson/xmip-core-transport-http).

Requests go on connections kept between them (`http::endpoint::Connections`, offering HTTP/1.1): the transport holds them and hands them to every client it makes, so a call costs one exchange and not a connect, a TLS handshake and a `Connection: close`, as it did until 2026-09-27.

A send target is read by `net::Target` in [xmip-core-library-net](https://github.com/IlleNilsson/xmip-core-library-net), the one reading of a URI every technology calls: scheme, authority, path and decoded query. Until 2026-09-28 it was read through the transport capability's `socket::target`, which split it on its first slash and left the query in the path.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
