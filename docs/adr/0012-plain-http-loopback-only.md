# 0012. Plain http only to this machine, unless a URL opts in

- Status: Accepted
- Date: 2026-11-23

## Context

AGENTS.md says every outbound call uses TLS. Phase 1 accepted `http://` for `catalog.uri` and
`storage.endpoint` to any host whenever no catalog credential was set, because iceblink-lab runs
Lakekeeper and MinIO on plain http. That left two gaps:

- S3 requests are SigV4-signed with real AWS credentials whether or not the endpoint is https.
  An http endpoint on another host sent those signed requests, and the table data they returned,
  across the network in plain text.
- A catalog on another host could be reached over http, so its answers (table locations,
  metadata) could be changed in transit.

Closing both must not break the common local setup where Iceblink runs in a container next to
Lakekeeper and MinIO and reaches them by service name, such as `http://minio:9000`, over a
private container network.

## Options

1. **https everywhere.** Matches AGENTS.md exactly, but every local lab would need TLS
   certificates for Lakekeeper and MinIO.
2. **Plain http only to this machine.** `http://` is accepted when the host is a loopback
   address (127.0.0.0/8, `[::1]`) or `localhost`; everything else must be https. Safe, but it
   refuses the container setup above.
3. **Option 2, plus a per-URL opt-in.** `[catalog] allow_plain_http = true` or
   `[storage] allow_plain_http = true` also accepts plain http to another host, for that one URL
   only. Off by default, logged as a warning at startup, and it never lets a catalog credential
   or SigV4 catalog signing go over plain http.
4. **Treat single-label names such as `minio` as local.** No setting, but a name like that can
   resolve anywhere through DNS search domains, so it would quietly reopen the gap.

## Decision

Option 3, accepted by the maintainer on 2026-11-23. The safe default closes both gaps, and the
opt-in keeps container setups working only where someone has written down that the network is
private. It is the same pattern as `storage.allow_local_files`.

## Consequences

- A config with `http://` to another host fails `iceblink config check` (exit 78) unless that URL's
  table sets `allow_plain_http = true`. The error names both fixes: https, or the opt-in.
- With the opt-in, S3 requests to that endpoint are still SigV4-signed: the signature is sent,
  never the secret key, but the requests and table data are readable on that network. Use it
  only on a network you control.
- A catalog credential or `[catalog.sigv4]` still requires an https `catalog.uri`, opt-in or not.
- The rule checks iceblink.toml only. URLs the catalog itself returns, such as a `uri` override in
  its `/v1/config` answer, aren't checked by it; whether iceberg-catalog-rest 0.11 follows such
  an override is worth checking in the v0.1 catalog work.
