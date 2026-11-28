//! TPC-H data, generated in-process with tpchgen (dbgen-identical rows) and written as Iceberg
//! tables in namespace `tpch`. Column types follow DuckDB's TPC-H schema, which the expected
//! answers came from: keys as bigint, money and quantity as decimal(15,2), dates as date.
//!
//! tpchgen-arrow targets other Arrow majors than Iceblink's 58, so the batches are built here.

use std::sync::Arc;

use datafusion::arrow::array::{
    ArrayRef, Date32Array, Decimal128Array, Int32Array, Int64Array, RecordBatch, StringArray,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::error::ArrowError;
use iceblink_testkit::Warehouse;
use tpchgen::generators::{
    CustomerGenerator, LineItemGenerator, NationGenerator, OrderGenerator, PartGenerator,
    PartSuppGenerator, RegionGenerator, SupplierGenerator,
};

use crate::SuiteError;

/// Generates all eight TPC-H tables at `scale_factor` into a new warehouse.
///
/// # Errors
///
/// An invalid batch, or a failure writing or committing a table.
pub async fn tpch(scale_factor: f64) -> Result<Warehouse, SuiteError> {
    let warehouse = Warehouse::new().await?;
    let tables = [
        ("nation", nation()?),
        ("region", region()?),
        ("part", part(scale_factor)?),
        ("supplier", supplier(scale_factor)?),
        ("partsupp", partsupp(scale_factor)?),
        ("customer", customer(scale_factor)?),
        ("orders", orders(scale_factor)?),
        ("lineitem", lineitem(scale_factor)?),
    ];
    for (name, batch) in tables {
        warehouse.add_table("tpch", name, &[batch]).await?;
    }
    Ok(warehouse)
}

/// Builds one batch column by column, all columns required.
#[derive(Default)]
struct Columns {
    fields: Vec<Field>,
    arrays: Vec<ArrayRef>,
}

impl Columns {
    fn push(mut self, name: &str, data_type: DataType, array: ArrayRef) -> Self {
        self.fields.push(Field::new(name, data_type, false));
        self.arrays.push(array);
        self
    }

    fn bigint(self, name: &str, values: impl Iterator<Item = i64>) -> Self {
        let array = Int64Array::from_iter_values(values);
        self.push(name, DataType::Int64, Arc::new(array))
    }

    fn int(self, name: &str, values: impl Iterator<Item = i32>) -> Self {
        let array = Int32Array::from_iter_values(values);
        self.push(name, DataType::Int32, Arc::new(array))
    }

    fn text(self, name: &str, values: impl Iterator<Item = String>) -> Self {
        let array = StringArray::from_iter_values(values);
        self.push(name, DataType::Utf8, Arc::new(array))
    }

    /// decimal(15,2) from hundredths.
    fn money(self, name: &str, cents: impl Iterator<Item = i64>) -> Result<Self, ArrowError> {
        let array = Decimal128Array::from_iter_values(cents.map(i128::from))
            .with_precision_and_scale(15, 2)?;
        Ok(self.push(name, DataType::Decimal128(15, 2), Arc::new(array)))
    }

    fn date(self, name: &str, epoch_days: impl Iterator<Item = i32>) -> Self {
        let array = Date32Array::from_iter_values(epoch_days);
        self.push(name, DataType::Date32, Arc::new(array))
    }

    fn build(self) -> Result<RecordBatch, ArrowError> {
        RecordBatch::try_new(Arc::new(Schema::new(self.fields)), self.arrays)
    }
}

fn nation() -> Result<RecordBatch, ArrowError> {
    let rows: Vec<_> = NationGenerator::default().iter().collect();
    Columns::default()
        .bigint("n_nationkey", rows.iter().map(|r| r.n_nationkey))
        .text("n_name", rows.iter().map(|r| r.n_name.to_owned()))
        .bigint("n_regionkey", rows.iter().map(|r| r.n_regionkey))
        .text("n_comment", rows.iter().map(|r| r.n_comment.to_owned()))
        .build()
}

fn region() -> Result<RecordBatch, ArrowError> {
    let rows: Vec<_> = RegionGenerator::default().iter().collect();
    Columns::default()
        .bigint("r_regionkey", rows.iter().map(|r| r.r_regionkey))
        .text("r_name", rows.iter().map(|r| r.r_name.to_owned()))
        .text("r_comment", rows.iter().map(|r| r.r_comment.to_owned()))
        .build()
}

fn part(scale_factor: f64) -> Result<RecordBatch, ArrowError> {
    let rows: Vec<_> = PartGenerator::new(scale_factor, 1, 1).iter().collect();
    Columns::default()
        .bigint("p_partkey", rows.iter().map(|r| r.p_partkey))
        .text("p_name", rows.iter().map(|r| r.p_name.to_string()))
        .text("p_mfgr", rows.iter().map(|r| r.p_mfgr.to_string()))
        .text("p_brand", rows.iter().map(|r| r.p_brand.to_string()))
        .text("p_type", rows.iter().map(|r| r.p_type.to_owned()))
        .int("p_size", rows.iter().map(|r| r.p_size))
        .text("p_container", rows.iter().map(|r| r.p_container.to_owned()))
        .money("p_retailprice", rows.iter().map(|r| r.p_retailprice.0))?
        .text("p_comment", rows.iter().map(|r| r.p_comment.to_owned()))
        .build()
}

fn supplier(scale_factor: f64) -> Result<RecordBatch, ArrowError> {
    let rows: Vec<_> = SupplierGenerator::new(scale_factor, 1, 1).iter().collect();
    Columns::default()
        .bigint("s_suppkey", rows.iter().map(|r| r.s_suppkey))
        .text("s_name", rows.iter().map(|r| r.s_name.to_string()))
        .text("s_address", rows.iter().map(|r| r.s_address.to_string()))
        .bigint("s_nationkey", rows.iter().map(|r| r.s_nationkey))
        .text("s_phone", rows.iter().map(|r| r.s_phone.to_string()))
        .money("s_acctbal", rows.iter().map(|r| r.s_acctbal.0))?
        .text("s_comment", rows.iter().map(|r| r.s_comment.clone()))
        .build()
}

fn partsupp(scale_factor: f64) -> Result<RecordBatch, ArrowError> {
    let rows: Vec<_> = PartSuppGenerator::new(scale_factor, 1, 1).iter().collect();
    Columns::default()
        .bigint("ps_partkey", rows.iter().map(|r| r.ps_partkey))
        .bigint("ps_suppkey", rows.iter().map(|r| r.ps_suppkey))
        .int("ps_availqty", rows.iter().map(|r| r.ps_availqty))
        .money("ps_supplycost", rows.iter().map(|r| r.ps_supplycost.0))?
        .text("ps_comment", rows.iter().map(|r| r.ps_comment.to_owned()))
        .build()
}

fn customer(scale_factor: f64) -> Result<RecordBatch, ArrowError> {
    let rows: Vec<_> = CustomerGenerator::new(scale_factor, 1, 1).iter().collect();
    Columns::default()
        .bigint("c_custkey", rows.iter().map(|r| r.c_custkey))
        .text("c_name", rows.iter().map(|r| r.c_name.to_string()))
        .text("c_address", rows.iter().map(|r| r.c_address.to_string()))
        .bigint("c_nationkey", rows.iter().map(|r| r.c_nationkey))
        .text("c_phone", rows.iter().map(|r| r.c_phone.to_string()))
        .money("c_acctbal", rows.iter().map(|r| r.c_acctbal.0))?
        .text(
            "c_mktsegment",
            rows.iter().map(|r| r.c_mktsegment.to_owned()),
        )
        .text("c_comment", rows.iter().map(|r| r.c_comment.to_owned()))
        .build()
}

fn orders(scale_factor: f64) -> Result<RecordBatch, ArrowError> {
    let rows: Vec<_> = OrderGenerator::new(scale_factor, 1, 1).iter().collect();
    Columns::default()
        .bigint("o_orderkey", rows.iter().map(|r| r.o_orderkey))
        .bigint("o_custkey", rows.iter().map(|r| r.o_custkey))
        .text(
            "o_orderstatus",
            rows.iter().map(|r| r.o_orderstatus.to_string()),
        )
        .money("o_totalprice", rows.iter().map(|r| r.o_totalprice.0))?
        .date(
            "o_orderdate",
            rows.iter().map(|r| r.o_orderdate.to_unix_epoch()),
        )
        .text(
            "o_orderpriority",
            rows.iter().map(|r| r.o_orderpriority.to_owned()),
        )
        .text("o_clerk", rows.iter().map(|r| r.o_clerk.to_string()))
        .int("o_shippriority", rows.iter().map(|r| r.o_shippriority))
        .text("o_comment", rows.iter().map(|r| r.o_comment.to_owned()))
        .build()
}

fn lineitem(scale_factor: f64) -> Result<RecordBatch, ArrowError> {
    let rows: Vec<_> = LineItemGenerator::new(scale_factor, 1, 1).iter().collect();
    Columns::default()
        .bigint("l_orderkey", rows.iter().map(|r| r.l_orderkey))
        .bigint("l_partkey", rows.iter().map(|r| r.l_partkey))
        .bigint("l_suppkey", rows.iter().map(|r| r.l_suppkey))
        .int("l_linenumber", rows.iter().map(|r| r.l_linenumber))
        .money("l_quantity", rows.iter().map(|r| r.l_quantity * 100))?
        .money("l_extendedprice", rows.iter().map(|r| r.l_extendedprice.0))?
        .money("l_discount", rows.iter().map(|r| r.l_discount.0))?
        .money("l_tax", rows.iter().map(|r| r.l_tax.0))?
        .text(
            "l_returnflag",
            rows.iter().map(|r| r.l_returnflag.to_owned()),
        )
        .text(
            "l_linestatus",
            rows.iter().map(|r| r.l_linestatus.to_owned()),
        )
        .date(
            "l_shipdate",
            rows.iter().map(|r| r.l_shipdate.to_unix_epoch()),
        )
        .date(
            "l_commitdate",
            rows.iter().map(|r| r.l_commitdate.to_unix_epoch()),
        )
        .date(
            "l_receiptdate",
            rows.iter().map(|r| r.l_receiptdate.to_unix_epoch()),
        )
        .text(
            "l_shipinstruct",
            rows.iter().map(|r| r.l_shipinstruct.to_owned()),
        )
        .text("l_shipmode", rows.iter().map(|r| r.l_shipmode.to_owned()))
        .text("l_comment", rows.iter().map(|r| r.l_comment.to_owned()))
        .build()
}
