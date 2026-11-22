//! Against a real AWS Glue catalog in ap-south-1. Ignored by default; run it by hand with AWS
//! credentials in the environment (AWS_PROFILE, SSO login, or keys):
//!
//! `FLOE_AWS=1 FLOE_AWS_ACCOUNT_ID=123456789012 cargo nextest run -p floe-catalog --test aws
//!  --run-ignored ignored-only --no-capture`
//!
//! `FLOE_AWS_PROFILE` picks a profile explicitly. The identity needs the read policy in
//! docs/aws-permissions.md.

use std::error::Error;
use std::io::{self, Write};

use floe_catalog::{
    CatalogSettings, CatalogTimeouts, LakeCatalog, SigV4Service, SigV4Settings, Table,
};
use url::Url;

#[tokio::test]
#[ignore = "needs a real AWS account; run with FLOE_AWS=1"]
async fn glue_lists_namespaces_and_loads_a_table() -> Result<(), Box<dyn Error>> {
    if std::env::var("FLOE_AWS").as_deref() != Ok("1") {
        return Ok(());
    }
    let catalog = LakeCatalog::connect(CatalogSettings {
        uri: Url::parse("https://glue.ap-south-1.amazonaws.com/iceberg")?,
        warehouse: std::env::var("FLOE_AWS_ACCOUNT_ID")?,
        credential: None,
        sigv4: Some(SigV4Settings {
            region: "ap-south-1".to_owned(),
            service: SigV4Service::Glue,
            profile: std::env::var("FLOE_AWS_PROFILE").ok(),
        }),
        timeouts: CatalogTimeouts::default(),
    })
    .await?;

    let namespaces = catalog.list_namespaces().await?;
    // This test reports what it read for a person to check, so it writes to stdout directly.
    let mut out = io::stdout().lock();
    writeln!(out, "namespaces: {}", namespaces.len())?;
    for namespace in &namespaces {
        let tables = catalog.list_tables(namespace).await?;
        writeln!(out, "  {namespace}: {} tables", tables.len())?;
        if let Some(name) = tables.first() {
            let table = catalog.load_table(name).await?;
            report(&mut out, &table)?;
            return Ok(());
        }
    }
    Err("no Iceberg table found in any namespace of this Glue catalog".into())
}

fn report(out: &mut impl Write, table: &Table) -> io::Result<()> {
    let metadata = table.metadata();
    writeln!(out, "loaded {}", table.identifier())?;
    writeln!(out, "  location: {}", metadata.location())?;
    writeln!(out, "  format version: {:?}", metadata.format_version())?;
    writeln!(
        out,
        "  columns: {}",
        metadata.current_schema().as_struct().fields().len()
    )?;
    writeln!(
        out,
        "  current snapshot: {:?}",
        metadata.current_snapshot_id()
    )
}
