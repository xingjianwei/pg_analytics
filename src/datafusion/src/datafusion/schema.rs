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

use async_std::sync::Mutex;
use async_std::task;
use datafusion::catalog::SchemaProvider;
use datafusion::common::DataFusionError;
use datafusion::datasource::TableProvider;
use datafusion::error::Result;
use pgrx::*;
use std::any::Any;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use supabase_wrappers::prelude::*;
use url::Url;

use crate::fdw::handler::*;
use crate::fdw::options::*;
use crate::schema::attribute::*;

use super::catalog::CatalogError;
use super::format::*;
use super::provider::*;
use super::session::*;

#[derive(Clone)]
pub struct LakehouseSchemaProvider {
    schema_name: String,
    tables: Arc<Mutex<HashMap<pg_sys::Oid, Arc<dyn TableProvider>>>>,
}

impl LakehouseSchemaProvider {
    pub fn new(schema_name: &str) -> Self {
        Self {
            schema_name: schema_name.to_string(),
            tables: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn table_impl(&self, table_name: &str) -> Result<Arc<dyn TableProvider>, CatalogError> {
        let pg_relation = unsafe {
            PgRelation::open_with_name(
                format!("\"{}\".\"{}\"", self.schema_name, table_name).as_str(),
            )
            .unwrap_or_else(|err| {
                panic!("{}", err);
            })
        };

        let table_options = pg_relation.table_options()?;
        let path = require_option(TableOption::Path.as_str(), &table_options)?;
        let extension = require_option(TableOption::Extension.as_str(), &table_options)?;
        let format = require_option_or(TableOption::Format.as_str(), &table_options, "");
        let mut tables = task::block_on(self.tables.lock());

        let table = match tables.entry(pg_relation.oid()) {
            Occupied(entry) => entry.into_mut(),
            Vacant(entry) => {
                let mut attribute_map: HashMap<usize, PgAttribute> = pg_relation
                    .tuple_desc()
                    .iter()
                    .enumerate()
                    .map(|(index, attribute)| {
                        (
                            index,
                            PgAttribute::new(attribute.name(), attribute.atttypid),
                        )
                    })
                    .collect();

                let url = Url::parse(path)?;

                if !Session::object_store_registry().contains_url(&url) {
                    let foreign_table = unsafe { pg_sys::GetForeignTable(pg_relation.oid()) };
                    let fdw_handler = FdwHandler::from(foreign_table);
                    let server_options = pg_relation.server_options()?;
                    let user_mapping_options = pg_relation.user_mapping_options()?;

                    register_object_store(
                        fdw_handler,
                        &url,
                        TableFormat::from(format),
                        server_options,
                        user_mapping_options,
                    )?;
                }

                let provider = match TableFormat::from(format) {
                    TableFormat::None => task::block_on(create_listing_provider(path, extension))?,
                    TableFormat::Delta => task::block_on(create_listing_provider(path, extension))?,
                };

                for (index, field) in provider.schema().fields().iter().enumerate() {
                    if let Some(attribute) = attribute_map.remove(&index) {
                        can_convert_to_attribute(field, attribute)?;
                    }
                }

                entry.insert(provider)
            }
        };

        let provider = match TableFormat::from(format) {
            TableFormat::Delta => table.clone(),
            _ => table.clone(),
        };

        Ok(provider)
    }
}

impl SchemaProvider for LakehouseSchemaProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    // This function never gets called anywhere, so it's safe to leave unimplemented
    fn table_names(&self) -> Vec<String> {
        todo!("table_names not implemented")
    }

    fn table<'life0, 'life1, 'async_trait>(
        &'life0 self,
        table_name: &'life1 str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<Arc<dyn TableProvider>>, DataFusionError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
        'life0: 'async_trait,
        'life1: 'async_trait,
    {
        Box::pin(async move {
            self.table_impl(table_name)
                .map(Some)
                .map_err(|err| DataFusionError::Execution(err.to_string()))
        })
    }

    fn table_exist(&self, table_name: &str) -> bool {
        let pg_relation = match unsafe {
            PgRelation::open_with_name(
                format!("\"{}\".\"{}\"", self.schema_name, table_name).as_str(),
            )
        } {
            Ok(relation) => relation,
            Err(_) => return false,
        };

        if !pg_relation.is_foreign_table() {
            return false;
        }

        let foreign_table = unsafe { pg_sys::GetForeignTable(pg_relation.oid()) };
        let foreign_server = unsafe { pg_sys::GetForeignServer((*foreign_table).serverid) };
        let fdw_handler = FdwHandler::from(foreign_server);

        fdw_handler != FdwHandler::Other
    }
}
