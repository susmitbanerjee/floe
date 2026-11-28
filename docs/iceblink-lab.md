# iceblink-lab: what Iceblink's gated tests expect

iceblink-lab is a separate repository with a docker compose stack: Lakekeeper as the Iceberg REST
catalog and MinIO as S3. Iceblink's ignored tests in `tests/lab.rs` run against it. This page is the
contract between the two; iceblink-lab provides everything below.

## Services

| Service | Address | Iceblink setting |
|---|---|---|
| Lakekeeper | `http://127.0.0.1:8181/catalog`, warehouse `demo` | `[catalog]` |
| MinIO | `http://127.0.0.1:9000`, bucket `warehouse`, path-style | `[storage]` |

The table `sales.orders` has columns `region` and `amount`, and 5 regions.

MinIO's keys reach Iceblink through the standard AWS chain: export `AWS_ACCESS_KEY_ID` and
`AWS_SECRET_ACCESS_KEY` before running the tests (the lab workflow reads them from iceblink-lab's
`iceblink.env`).

## Files under the checkout (`ICEBLINK_LAB_DIR`)

| File | Written by | Holds |
|---|---|---|
| `checks/region.csv` | iceblink-lab's pyiceberg check script | `region,count,sum` header, then one row per region, ordered by region |
| `checks/region_files_scanned.txt` | the same script | the number of data files pyiceberg plans for the region query, as one integer |
| `checks/athena_region.csv` | committed after an Athena run on the AWS Glue copy | the same columns and order as `region.csv` |
| `iceblink.toml` | iceblink-lab | the config the lab workflow starts `iceblink serve` with |
| `iceblink.env` | iceblink-lab | MinIO's keys as `AWS_ACCESS_KEY_ID=…` and `AWS_SECRET_ACCESS_KEY=…` lines (lab only, not secrets) |

The query is
`SELECT region, count(*), sum(amount) FROM sales.orders GROUP BY region ORDER BY region`.
Sums compare within 0.01; an empty field means NULL.

## Running

- **Locally, Iceblink in-process:**
  ```bash
  ICEBLINK_LAB=1 ICEBLINK_LAB_DIR=../iceblink-lab cargo nextest run -p iceblink-suites --test lab --run-ignored ignored-only
  ```
- **Against a running `iceblink serve`:** also set `ICEBLINK_LAB_ADDR=127.0.0.1:5432`.
- **AWS:** `ICEBLINK_AWS=1` with:
  - `ICEBLINK_AWS_ACCOUNT_ID` (the Glue catalog);
  - `ICEBLINK_AWS_PREFIX` (`s3://bucket/prefix/` holding the tables);
  - optionally `ICEBLINK_AWS_REGION` (default `ap-south-1`) and `ICEBLINK_AWS_PROFILE`;
  - credentials in the AWS chain.

`.github/workflows/lab.yml` runs the lab tests nightly and on demand once the repository
variable `ICEBLINK_LAB_REPO` names iceblink-lab. Until then it skips with a notice.
