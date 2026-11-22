//! Lab test against floe-lab's Lakekeeper. Ignored by default; run it with floe-lab up:
//! `FLOE_LAB=1 cargo nextest run -p floe-catalog --run-ignored ignored-only`.

use std::error::Error;

use floe_catalog::{CatalogSettings, CatalogTimeouts, LakeCatalog, NamespaceName};
use url::Url;

#[tokio::test]
#[ignore = "needs floe-lab; run with FLOE_LAB=1"]
async fn lakekeeper_lists_orders_in_sales() -> Result<(), Box<dyn Error>> {
    if std::env::var("FLOE_LAB").as_deref() != Ok("1") {
        return Ok(());
    }
    let catalog = LakeCatalog::connect(CatalogSettings {
        uri: Url::parse("http://127.0.0.1:8181/catalog")?,
        warehouse: "demo".to_owned(),
        credential: None,
        sigv4: None,
        timeouts: CatalogTimeouts::default(),
    })
    .await?;
    let tables = catalog.list_tables(&NamespaceName::new("sales")?).await?;
    assert!(
        tables.iter().any(|table| table.name() == "orders"),
        "sales has no orders table: {tables:?}"
    );
    Ok(())
}
