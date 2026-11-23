# 0012. Plain http only to this machine

- Status: Proposed
- Date: 2026-11-23

## Context

AGENTS.md says every outbound call uses TLS. Phase 1 accepted `http://` for `catalog.uri` and
`storage.endpoint` to any host whenever no catalog credential was set, because floe-lab runs
Lakekeeper and MinIO on plain http. That left two gaps:

- S3 requests are SigV4-signed with real AWS credentials whether or not the endpoint is https.
  An http endpoint on another host sent those signed requests, and the table data they returned,
  across the network in plain text.
- A catalog on another host could be reached over http, so its answers (table locations,
  metadata) could be changed in transit.

## Options

1. **https everywhere.** Matches AGENTS.md exactly, but floe-lab would need TLS certificates for
   Lakekeeper and MinIO, which makes the local lab much harder to start.
2. **Plain http only to this machine.** `http://` is accepted when the host is a loopback
   address (127.0.0.0/8, `[::1]`) or `localhost`. Everything else must be https. Traffic that
   stays on the machine never crosses a network.
3. **Keep the phase 1 rule, and warn on remote http.** Nobody reads warnings, and the gap stays
   open.

## Decision

Option 2: it closes both gaps and keeps floe-lab unchanged, since floe-lab publishes its
services on 127.0.0.1. It is the same line phase 1 draws for the Postgres listener. Awaiting the
maintainer's sign-off.

## Consequences

- A config with `http://` to another host now fails `floe config check` (exit 78). The error
  says to switch to https, or to `http://127.0.0.1` when the service runs on this machine.
- Running Floe in a container next to Lakekeeper or MinIO, reached by service name over http,
  no longer works: publish the services on the host's loopback, or put TLS in front of them.
- The rule checks floe.toml only. URLs the catalog itself returns, such as a `uri` override in
  its `/v1/config` answer, aren't checked by it; whether iceberg-catalog-rest 0.11 follows such
  an override is worth checking in the v0.1 catalog work.
