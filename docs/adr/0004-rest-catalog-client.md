# 0004. Wrapping iceberg-catalog-rest 0.11 in LakeCatalog

- Status: Accepted
- Date: 2026-11-19

## Context

The catalog prompt asks floe-catalog to wrap iceberg-rust's REST client (0.11.0, pinned by
ADR 0001) in a small read-only `LakeCatalog`, with Floe's own timeouts, retries, bearer auth and
error variants. Reading the client's source showed where it differs from what the prompt
assumed:

1. **HTTP status codes aren't exposed.** Only 404 gets its own error kind (`TableNotFound`,
   `NamespaceNotFound`). Every other status is `ErrorKind::Unexpected` with the message "Received
   response with unexpected status code". The status sits in the error's private context, and
   `iceberg::Error` offers only `kind()`, `message()`, `retryable()` and `source()`. Telling 401
   from 429 or 503 can't be done "by kind" alone.
2. **`load_table` needs a `StorageFactory`** and refuses to run without one, because every
   `Table` carries a FileIO. Storage belongs to floe-scan, and iceberg's `Storage` trait can write
   and delete.
3. **`load()` is lazy.** The `/v1/config` handshake runs on the first request, so a builder that
   loads fine can still have a wrong URI or token.
4. **The client sets `user-agent: iceberg-rs/0.11.0` on every request**, overriding any default
   on the `reqwest::Client`.
5. **A custom `reqwest::Client` is accepted** (`RestCatalogBuilder::with_client`) and used for
   every request.
6. **Paused time and real sockets don't mix.** wiremock serves from its own thread, and tokio's
   paused clock auto-advances while a request waits on the network, so a pending request
   timeout fires at once.

## Options

- For 1: (a) parse `status: NNN` out of the error's Display text; (b) write Floe's own REST
  client; (c) wait for iceberg-rust to expose the context.
- For 2: (a) a refusing, metadata-only `StorageFactory`; (b) iceberg's `MemoryStorageFactory`;
  (c) wire S3 storage into floe-catalog now.
- For TLS roots: (a) compiled-in webpki-roots; (b) the OS trust store through
  rustls-native-certs, which reads certificate files at runtime.

## Decision

- **Timeouts on the client:** connect 5 s and total 30 s per HTTP request, set on the
  `reqwest::Client` that Floe passes in. No `tokio::time::timeout` wrapper is needed. A
  paginated listing makes several requests, each with its own 30 s.
- **Statuses: option 1a.** `error::http_status` reads the status only when the kind and message
  match the client's unexpected-status error, and only ever uses the number, never the headers
  or body in the same text. Unit tests pin the format. wiremock tests (401, 403, 503) pin it
  against the real client, so a change in iceberg's Display fails the build.
- **Storage: option 2a, chosen by the maintainer.** `MetadataOnlyStorage` fails in `build()`,
  and FileIO only calls `build()` on first use. So `load_table` returns metadata, and every read,
  write or delete through the table's FileIO fails before touching anything. floe-scan will
  attach read-only storage.
- **`connect` probes** with one namespace listing, so misconfiguration fails at startup.
- **User agent:** the `header.user-agent = floe/<version>` property overrides iceberg's.
- **Auth:** a Floe `AuthManager` sends `catalog.credential` as `Authorization: Bearer …`, from a
  `SecretString`, as a sensitive header value. It is never a plain iceberg property.
- **TLS: option a, chosen by the maintainer.** reqwest 0.12 `rustls-tls` with ring and
  webpki-roots (licence exception in deny.toml). Private CAs aren't trusted; revisit if a
  deployment needs them. Redirects are off, so a catalog can't send the token elsewhere.
- **Retries, as the maintainer read the prompt:** 3 attempts in total, waiting 200 ms and then
  400 ms. Only list and load retry, on timeouts, 429 and 5xx. `connect` doesn't retry.
- **Tests:** paused time only where it is safe: the retry helper without any network; the 503
  test on a client without a request timeout; and the timeout test, where the timeout firing is
  the point.

## Consequences

- An iceberg-rust upgrade must rerun the status tests. If the Display format changes they fail,
  and the fix is in one function.
- Ask upstream to expose the error context or status. When it does, drop the parse and
  supersede this ADR.
- Until floe-scan attaches storage, a loaded table's FileIO refuses everything, by design.
- A dead catalog costs up to 3 × 30 s plus 0.6 s of backoff before a read gives up.
