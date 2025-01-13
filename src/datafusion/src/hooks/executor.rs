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

use anyhow::Result;
use async_std::task;
use datafusion::common::arrow::array::RecordBatch;
use datafusion::logical_expr::LogicalPlan;
use pgrx::*;
use std::ffi::CStr;
use thiserror::Error;

use crate::datafusion::context::ContextError;
use crate::datafusion::plan::QueryString;
use crate::datafusion::session::Session;
use crate::schema::cell::*;

use super::query::*;

macro_rules! fallback_warning {
    ($msg:expr) => {
        warning!("This query was not fully pushed down to DataFusion because DataFusion returned an error: {}. Query times may be impacted. Please submit a request at https://github.com/paradedb/paradedb/issues if you would like to see this query pushed down.", $msg);
    };
}

#[allow(deprecated)]
pub async fn executor_run(
    query_desc: PgBox<pg_sys::QueryDesc>,
    direction: pg_sys::ScanDirection::Type,
    count: u64,
    execute_once: bool,
    prev_hook: fn(
        query_desc: PgBox<pg_sys::QueryDesc>,
        direction: pg_sys::ScanDirection::Type,
        count: u64,
        execute_once: bool,
    ) -> HookResult<()>,
) -> Result<()> {
    let ps = query_desc.plannedstmt;
    let rtable = unsafe { (*ps).rtable };
    let query = get_current_query(ps, unsafe { CStr::from_ptr(query_desc.sourceText) })?;
    let query_type = get_query_type(ps);

    if rtable.is_null()
        || query_desc.operation != pg_sys::CmdType::CMD_SELECT
        || query_type != QueryType::DataFusion
        // Tech Debt: Find a less hacky way to let COPY go through
        || query.to_lowercase().starts_with("copy")
    {
        prev_hook(query_desc, direction, count, execute_once);
        return Ok(());
    }

    // Parse the query into a LogicalPlan
    match LogicalPlan::try_from(QueryString(&query)) {
        Ok(logical_plan) => {
            // Don't intercept any DDL or DML statements
            match logical_plan {
                LogicalPlan::Ddl(_)
                | LogicalPlan::Dml(_)
                | LogicalPlan::Explain(_)
                | LogicalPlan::Analyze(_) => {
                    prev_hook(query_desc, direction, count, execute_once);
                    return Ok(());
                }
                _ => {}
            };

            // Execute SELECT
            match task::block_on(get_datafusion_batches(logical_plan)) {
                Ok(batches) => write_batches_to_slots(query_desc, batches)?,
                Err(err) => {
                    fallback_warning!(err.to_string());
                    prev_hook(query_desc, direction, count, execute_once);
                    return Ok(());
                }
            };
        }
        Err(err) => {
            fallback_warning!(err.to_string());
            prev_hook(query_desc, direction, count, execute_once);
        }
    };

    Ok(())
}

#[inline]
async fn get_datafusion_batches(
    logical_plan: LogicalPlan,
) -> Result<Vec<RecordBatch>, ContextError> {
    // Execute the logical plan and collect the resulting batches
    let context = Session::session_context()?;
    let dataframe = context.execute_logical_plan(logical_plan).await?;
    Ok(dataframe.collect().await?)
}

#[inline]
fn write_batches_to_slots(
    query_desc: PgBox<pg_sys::QueryDesc>,
    mut batches: Vec<RecordBatch>,
) -> Result<(), ExecutorHookError> {
    // Convert the DataFusion batches to Postgres tuples and send them to the destination
    unsafe {
        let tuple_desc = PgTupleDesc::from_pg(query_desc.tupDesc);
        let estate = query_desc.estate;
        (*estate).es_processed = 0;

        let dest = query_desc.dest;
        let startup = (*dest)
            .rStartup
            .ok_or(ExecutorHookError::RStartupNotFound)?;
        startup(dest, query_desc.operation as i32, query_desc.tupDesc);

        let receive = (*dest)
            .receiveSlot
            .ok_or(ExecutorHookError::ReceiveSlotNotFound)?;

        for batch in batches.iter_mut() {
            for row_index in 0..batch.num_rows() {
                let tuple_table_slot =
                    pg_sys::MakeTupleTableSlot(query_desc.tupDesc, &pg_sys::TTSOpsVirtual);

                pg_sys::ExecStoreVirtualTuple(tuple_table_slot);

                for (col_index, _) in tuple_desc.iter().enumerate() {
                    let attribute = tuple_desc
                        .get(col_index)
                        .ok_or(ExecutorHookError::AttributeNotFound(col_index))?;
                    let column = batch.column(col_index);
                    let tts_value = (*tuple_table_slot).tts_values.add(col_index);
                    let tts_isnull = (*tuple_table_slot).tts_isnull.add(col_index);

                    match column.get_cell(row_index, attribute.atttypid, attribute.type_mod())? {
                        Some(cell) => {
                            if let Some(datum) = cell.into_datum() {
                                *tts_value = datum;
                            }
                        }
                        None => {
                            *tts_isnull = true;
                        }
                    };
                }

                receive(tuple_table_slot, dest);
                (*estate).es_processed += 1;
                pg_sys::ExecDropSingleTupleTableSlot(tuple_table_slot);
            }
        }

        let shutdown = (*dest)
            .rShutdown
            .ok_or(ExecutorHookError::RShutdownNotFound)?;
        shutdown(dest);
    }

    Ok(())
}

#[derive(Error, Debug)]
pub enum ExecutorHookError {
    #[error(transparent)]
    ContextError(#[from] ContextError),

    #[error(transparent)]
    DataTypeError(#[from] DataTypeError),

    #[error(transparent)]
    Utf8Error(#[from] std::str::Utf8Error),

    #[error("Could not find attribute {0} in tuple descriptor")]
    AttributeNotFound(usize),

    #[error("Unexpected error: rShutdown not found")]
    RShutdownNotFound,

    #[error("Unexpected error: receiveSlot not found")]
    ReceiveSlotNotFound,

    #[error("Unexpected error: rStartup not found")]
    RStartupNotFound,
}
