# AWS permissions for Iceblink

Iceblink only reads. Give the AWS identity it signs with (the profile, SSO role, or instance or pod
role) this policy and nothing more. Replace `REGION`, `ACCOUNT_ID`, `BUCKET` and `PREFIX` (the
warehouse location, without a trailing slash).

## Glue Data Catalog (default catalog)

What each catalog call needs, per AWS's Glue Iceberg REST API reference:

| Iceblink call | REST route | IAM action |
|---|---|---|
| `connect` | `GET /iceberg/v1/config?warehouse=ACCOUNT_ID` | `glue:GetCatalog` |
| `list_namespaces` | `GET /iceberg/v1/catalogs/{catalog}/namespaces` | `glue:GetDatabase`, `glue:GetDatabases` |
| `list_tables` | `GET …/namespaces/{ns}/tables` | `glue:GetTables` |
| `load_table` | `GET …/namespaces/{ns}/tables/{table}` | `glue:GetTable` |

Reading the table's files, done by the object store client, needs `s3:GetObject` on the data and
`s3:ListBucket` on the warehouse prefix.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "IceblinkReadGlueCatalog",
      "Effect": "Allow",
      "Action": [
        "glue:GetCatalog",
        "glue:GetDatabase",
        "glue:GetDatabases",
        "glue:GetTable",
        "glue:GetTables"
      ],
      "Resource": [
        "arn:aws:glue:REGION:ACCOUNT_ID:catalog",
        "arn:aws:glue:REGION:ACCOUNT_ID:database/*",
        "arn:aws:glue:REGION:ACCOUNT_ID:table/*/*"
      ]
    },
    {
      "Sid": "IceblinkListWarehousePrefix",
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::BUCKET",
      "Condition": { "StringLike": { "s3:prefix": ["PREFIX/*"] } }
    },
    {
      "Sid": "IceblinkReadWarehouseObjects",
      "Effect": "Allow",
      "Action": "s3:GetObject",
      "Resource": "arn:aws:s3:::BUCKET/PREFIX/*"
    }
  ]
}
```

To narrow further, replace `database/*` and `table/*/*` with the databases and tables Iceblink should
see, such as `database/sales` and `table/sales/*`.

## S3 Tables through Glue

Set `warehouse = "ACCOUNT_ID:s3tablescatalog/TABLE_BUCKET"` and use the federated catalog ARNs
in the Glue statement instead:

```text
arn:aws:glue:REGION:ACCOUNT_ID:catalog
arn:aws:glue:REGION:ACCOUNT_ID:catalog/s3tablescatalog
arn:aws:glue:REGION:ACCOUNT_ID:catalog/s3tablescatalog/TABLE_BUCKET
arn:aws:glue:REGION:ACCOUNT_ID:database/s3tablescatalog/TABLE_BUCKET/*
arn:aws:glue:REGION:ACCOUNT_ID:table/s3tablescatalog/TABLE_BUCKET/*/*
```

S3 Tables data is served through Lake Formation or S3 Tables' own permissions, not plain
`s3:GetObject`. Lake Formation credential vending is out of scope for now, so this covers the
catalog side only.

## What this leaves out on purpose

- Nothing that writes: no `glue:Create*`, `glue:Update*`, `glue:Delete*`, no `s3:PutObject` or
  `s3:DeleteObject`.
- Lake Formation: if the catalog is governed by Lake Formation, the identity also needs
  `DESCRIBE` on the databases and `SELECT`/`DESCRIBE` on the tables there.
- Cross-account access and assumed roles beyond what the profile or environment already set up.
