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

use async_std::stream::StreamExt;
use async_std::task;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::physical_plan::SendableRecordBatchStream;
use datafusion::prelude::DataFrame;
use object_store_opendal::OpendalStore;
use opendal::services::S3;
use opendal::Operator;
use pgrx::*;
use std::collections::HashMap;
use std::sync::Arc;
use supabase_wrappers::prelude::*;
use url::Url;

use crate::datafusion::context::ContextError;
use crate::datafusion::format::TableFormat;
use crate::datafusion::session::Session;
use crate::fdw::options::*;

use super::base::*;

#[wrappers_fdw(
    author = "ParadeDB",
    website = "https://github.com/paradedb/paradedb",
    error_type = "BaseFdwError"
)]
pub(crate) struct S3Fdw {
    dataframe: Option<DataFrame>,
    stream: Option<SendableRecordBatchStream>,
    current_batch: Option<RecordBatch>,
    current_batch_index: usize,
    target_columns: Vec<Column>,
}

pub enum AmazonServerOption {
    Endpoint,
    Region,
    AllowAnonymous,
}

impl AmazonServerOption {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Endpoint => "endpoint",
            Self::Region => "region",
            Self::AllowAnonymous => "allow_anonymous",
        }
    }

    pub fn is_required(&self) -> bool {
        match self {
            Self::Endpoint => false,
            Self::Region => false,
            Self::AllowAnonymous => false,
        }
    }

    pub fn iter() -> impl Iterator<Item = Self> {
        [Self::Endpoint, Self::Region, Self::AllowAnonymous].into_iter()
    }
}

pub enum AmazonUserMappingOption {
    AccessKeyId,
    SecretAccessKey,
    SecurityToken,
}

impl AmazonUserMappingOption {
    pub fn as_str(&self) -> &str {
        match self {
            Self::AccessKeyId => "access_key_id",
            Self::SecretAccessKey => "secret_access_key",
            Self::SecurityToken => "security_token",
        }
    }
}

impl TryFrom<ObjectStoreConfig> for S3 {
    type Error = ContextError;

    fn try_from(options: ObjectStoreConfig) -> Result<Self, Self::Error> {
        let url = options.url();
        let server_options = options.server_options();
        let user_mapping_options = options.user_mapping_options();

        let mut builder = S3::default();
        builder.disable_config_load();
        builder.disable_ec2_metadata();

        if let Some(bucket) = url.host_str() {
            builder.bucket(bucket);
        }

        if let Some(region) = server_options.get(AmazonServerOption::Region.as_str()) {
            builder.region(region);
        }

        if let Some(access_key_id) =
            user_mapping_options.get(AmazonUserMappingOption::AccessKeyId.as_str())
        {
            builder.access_key_id(access_key_id);
        }

        if let Some(secret_access_key) =
            user_mapping_options.get(AmazonUserMappingOption::SecretAccessKey.as_str())
        {
            builder.secret_access_key(secret_access_key);
        }

        if let Some(security_token) =
            user_mapping_options.get(AmazonUserMappingOption::SecurityToken.as_str())
        {
            builder.security_token(security_token);
        }

        if let Some(endpoint) = server_options.get(AmazonServerOption::Endpoint.as_str()) {
            builder.endpoint(endpoint);
        }

        if let Some(allow_anonymous) =
            server_options.get(AmazonServerOption::AllowAnonymous.as_str())
        {
            if allow_anonymous == "true" {
                builder.allow_anonymous();
            }
        }

        Ok(builder)
    }
}

impl BaseFdw for S3Fdw {
    fn register_object_store(
        url: &Url,
        format: TableFormat,
        server_options: HashMap<String, String>,
        user_mapping_options: HashMap<String, String>,
    ) -> Result<(), ContextError> {
        let context = Session::session_context()?;

        let builder = S3::try_from(ObjectStoreConfig::new(
            url,
            format,
            server_options.clone(),
            user_mapping_options.clone(),
        ))?;

        let operator = Operator::new(builder)?.finish();
        let object_store = Arc::new(OpendalStore::new(operator));

        context
            .runtime_env()
            .register_object_store(url, object_store);

        Ok(())
    }

    fn get_current_batch(&self) -> Option<RecordBatch> {
        self.current_batch.clone()
    }

    fn get_current_batch_index(&self) -> usize {
        self.current_batch_index
    }

    fn get_target_columns(&self) -> Vec<Column> {
        self.target_columns.clone()
    }

    fn set_current_batch(&mut self, batch: Option<RecordBatch>) {
        self.current_batch = batch;
    }

    fn set_current_batch_index(&mut self, index: usize) {
        self.current_batch_index = index;
    }

    fn set_dataframe(&mut self, dataframe: DataFrame) {
        self.dataframe = Some(dataframe);
    }

    async fn create_stream(&mut self) -> Result<(), BaseFdwError> {
        if self.stream.is_none() {
            self.stream = Some(
                self.dataframe
                    .clone()
                    .ok_or(BaseFdwError::DataFrameNotFound)?
                    .execute_stream()
                    .await?,
            );
        }

        Ok(())
    }

    fn clear_stream(&mut self) {
        self.stream = None;
    }

    fn set_target_columns(&mut self, columns: &[Column]) {
        self.target_columns = columns.to_vec();
    }

    async fn get_next_batch(&mut self) -> Result<Option<RecordBatch>, BaseFdwError> {
        match self
            .stream
            .as_mut()
            .ok_or(BaseFdwError::StreamNotFound)?
            .next()
            .await
        {
            Some(Ok(batch)) => Ok(Some(batch)),
            None => Ok(None),
            Some(Err(err)) => Err(BaseFdwError::DataFusionError(err)),
        }
    }
}

impl ForeignDataWrapper<BaseFdwError> for S3Fdw {
    fn new(
        table_options: HashMap<String, String>,
        server_options: HashMap<String, String>,
        user_mapping_options: HashMap<String, String>,
    ) -> Result<Self, BaseFdwError> {
        let path = require_option(TableOption::Path.as_str(), &table_options)?;
        let format = require_option_or(TableOption::Format.as_str(), &table_options, "");

        S3Fdw::register_object_store(
            &Url::parse(path)?,
            TableFormat::from(format),
            server_options,
            user_mapping_options,
        )?;

        Ok(Self {
            dataframe: None,
            current_batch: None,
            current_batch_index: 0,
            stream: None,
            target_columns: Vec::new(),
        })
    }

    fn validator(
        opt_list: Vec<Option<String>>,
        catalog: Option<pg_sys::Oid>,
    ) -> Result<(), BaseFdwError> {
        if let Some(oid) = catalog {
            match oid {
                FOREIGN_DATA_WRAPPER_RELATION_ID => {}
                FOREIGN_SERVER_RELATION_ID => {
                    let valid_options: Vec<String> = AmazonServerOption::iter()
                        .map(|opt| opt.as_str().to_string())
                        .collect();

                    validate_options(opt_list.clone(), valid_options)?;

                    for opt in AmazonServerOption::iter() {
                        if opt.is_required() {
                            check_options_contain(&opt_list, opt.as_str())?;
                        }
                    }
                }
                FOREIGN_TABLE_RELATION_ID => {
                    let valid_options: Vec<String> = TableOption::iter()
                        .map(|opt| opt.as_str().to_string())
                        .collect();

                    validate_options(opt_list.clone(), valid_options)?;

                    for opt in TableOption::iter() {
                        if opt.is_required() {
                            check_options_contain(&opt_list, opt.as_str())?;
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn begin_scan(
        &mut self,
        _quals: &[Qual],
        columns: &[Column],
        _sorts: &[Sort],
        limit: &Option<Limit>,
        options: HashMap<String, String>,
    ) -> Result<(), BaseFdwError> {
        task::block_on(self.begin_scan_impl(_quals, columns, _sorts, limit, options))
            .expect("begin_scan failed");
        Ok(())
    }

    fn iter_scan(&mut self, row: &mut Row) -> Result<Option<()>, BaseFdwError> {
        task::block_on(self.iter_scan_impl(row))
    }

    fn end_scan(&mut self) -> Result<(), BaseFdwError> {
        self.end_scan_impl()
    }
    fn explain(&self) -> Result<Option<Vec<(String, String)>>, BaseFdwError> {
        Ok(Some(vec![]))
    }
}
