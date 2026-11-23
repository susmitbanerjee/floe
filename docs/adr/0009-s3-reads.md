# 0009. S3 reads with Floe's own read-only reader

- Status: Accepted
- Date: 2026-11-20

## Context

Prompt 9 asked `floe serve` to answer the floe-lab region query (5 rows from `sales.orders` on
MinIO). That differed from where the code was:
- floe-scan read only local files, because S3 was deferred in Prompt 6;
- iceberg 0.11's core crate ships only local and in-memory storage.

So the query needed an S3 reader first.

What Floe needs from S3 is small:
- `HEAD` for sizes;
- ranged `GET` for Parquet footers and column chunks;
- whole `GET` for manifests;
- AWS S3 and S3-compatible stores (MinIO), path-style or virtual-hosted;
- credentials from the standard AWS chain, which floe.toml already promises.

It must never write.

## Options

1. **object_store 0.13.2's S3 client** (`aws` feature). Already in the tree through DataFusion
   54. Chosen first, then dropped:
   - `cargo deny` fails on two advisories in its quick-xml 0.39.4: RUSTSEC-2026-0194 (CPU) and
     RUSTSEC-2026-0195 (memory), both triggered by crafted XML from the server.
   - No 0.13 release has the fix (quick-xml 0.41 or later).
   - Its reqwest dependency also switches on native-roots TLS for the whole build.
2. **object_store 0.14.2**, which has the fixed quick-xml. Its `aws` feature turns on reqwest
   0.13's `rustls` feature, which brings:
   - **aws-lc-rs:** the C crypto build ADRs 0001 and 0005 kept out;
   - **rustls-platform-verifier:** the system trust store, against ADR 0004's webpki-only
     roots;
   - **a second reqwest major.**

   None of it can be turned off.
3. **Ignore the advisories** with an expiry, on the grounds that the XML only comes from the
   configured S3 endpoint.
4. **Floe's own read-only reader** on reqwest 0.12 and aws-sigv4, both already dependencies.
   - **Pros:** no new crates, no XML parsing, ring and webpki roots kept.
   - **Cons:** about 350 lines to own.

## Decision

**Option 4, maintainer's choice after options 1 and 2 hit the issues above.**
`floe-scan/src/s3.rs` implements iceberg's `Storage` directly.

- **Read-only by construction.**
  - The module builds only GET and HEAD requests.
  - Writes, deletes and output files fail before any request, behind `CountingStorage`'s own
    refusal.
  - A test checks that no request at all reaches the endpoint for them.
- **No response parsing.** Only status codes and `Content-Length` are read; error bodies are
  never parsed. Status codes map to fixes:
  - 403: check that the AWS identity may `s3:GetObject`;
  - 404: missing object;
  - 301, 307 and 400: check `storage.region` and `storage.endpoint`.
- **URLs.**
  - AWS is virtual-hosted (`bucket.s3.region.amazonaws.com`), or path-style when `path_style`.
  - A custom endpoint is `endpoint/bucket/key` (path-style, MinIO) or `bucket.endpoint/key`.
  - Each key segment is URI-encoded once, as S3 expects.
- **Signing.** SigV4 through aws-sigv4, service `s3`, with:
  - `UNSIGNED-PAYLOAD`;
  - single path encoding and no path normalisation;
  - `host`, `x-amz-date`, `x-amz-content-sha256` and the session token signed;
  - the `Range` header left unsigned, as SigV4 allows.
- **HTTP.**
  - reqwest 0.12 with rustls on ring and only the compiled-in webpki roots, named explicitly.
    floe-catalog's client now names them explicitly too.
  - No redirects; connect 5 s, request 60 s.
- **Retries:** only these reads, for timeouts, failed connections, 429 and 5xx. Three attempts,
  waiting 200 ms and then 400 ms.
- **Credentials.**
  - They come from `floe_catalog::AwsCredentials`, the cache the catalog's SigV4 signer uses,
    moved out of the signer unchanged.
  - The chain loads on the first read, so `floe serve` starts without touching AWS.
  - It uses `[catalog.sigv4] profile` when set (added 2026-11-23: before, S3 reads ignored it
    and could run as a different identity from the catalog), and the default chain otherwise.
- **Locations.** `Storages` routes `s3://` and `s3a://` to one shared `S3Storage` per catalog,
  and `file://` to the local filesystem (tests only: the location guard keeps it to allowed
  prefixes). Any other scheme is refused, naming it.

## Consequences

- **The tree has no XML parser,** and no new crates were added for S3.
- **A missing or wrong AWS identity shows on the first query, not at startup.** The client gets
  XX000 with a request ID, and the log line under that ID says to set AWS_PROFILE or the
  AWS_ACCESS_KEY_ID pair. A startup check could come with `floe doctor` (v0.1).
- **Not supported:** multipart parallel range fetching, S3 Express, and Lakekeeper-vended
  credentials (`X-Iceberg-Access-Delegation`). Revisit if a catalog only hands out per-table
  credentials.
- **Tested without S3:** wiremock as a path-style endpoint covers:
  - HEAD, ranged and whole GETs, and a store that ignores `Range`;
  - SigV4 headers;
  - retries on 503 but not on 403;
  - 404 and 403;
  - zero writes;
  - every URL style with key encoding.

  Real MinIO and AWS S3 are covered only by the floe-lab test (`FLOE_LAB=1`).
