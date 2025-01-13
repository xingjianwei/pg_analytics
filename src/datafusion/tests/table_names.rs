// Copyright (c) 2023-2024 Retake, Inc.
//
// This file is part of ParadeDB - Postgres for Search and Analytics
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

mod fixtures;

use anyhow::Result;
use datafusion::parquet::arrow::ArrowWriter;
use fixtures::*;
use rstest::*;
use shared::fixtures::arrow::{primitive_record_batch, primitive_setup_fdw_local_file_listing};
use shared::fixtures::tempfile::TempDir;
use sqlx::PgConnection;
use std::fs::File;

#[rstest]
async fn test_table_case_sensitivity(mut conn: PgConnection, tempdir: TempDir) -> Result<()> {
    let stored_batch = primitive_record_batch()?;
    let parquet_path = tempdir.path().join("test_arrow_types.parquet");
    let parquet_file = File::create(&parquet_path)?;

    let mut writer = ArrowWriter::try_new(parquet_file, stored_batch.schema(), None).unwrap();
    writer.write(&stored_batch)?;
    writer.close()?;

    primitive_setup_fdw_local_file_listing(
        parquet_path.as_path().to_str().unwrap(),
        "parquet",
        "\"PrimitiveTable\"",
    )
    .execute(&mut conn);

    let retrieved_batch =
        "SELECT * FROM \"PrimitiveTable\"".fetch_recordbatch(&mut conn, &stored_batch.schema());

    assert_eq!(stored_batch.num_columns(), retrieved_batch.num_columns());

    let retrieved_batch = "SELECT * FROM public.\"PrimitiveTable\""
        .fetch_recordbatch(&mut conn, &stored_batch.schema());

    assert_eq!(stored_batch.num_columns(), retrieved_batch.num_columns());

    let retrieved_batch = "SELECT * FROM \"public\".\"PrimitiveTable\""
        .fetch_recordbatch(&mut conn, &stored_batch.schema());

    assert_eq!(stored_batch.num_columns(), retrieved_batch.num_columns());

    Ok(())
}
