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

mod bench_hits;
mod benchmark;
mod cli;
mod elastic;
mod subcommand;
mod tables;

use anyhow::Result;
use async_std::task::block_on;
use cli::{Cli, Corpus, EsLogsCommand, HitsCommand, Subcommand};
use dotenvy::dotenv;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load env vars from a .env in any parent folder.
    dotenv().ok();

    let cli = Cli::default();
    match cli.subcommand {
        Subcommand::Install => subcommand::install(),
        Subcommand::Bench(bench) => match bench.corpus {
            Corpus::Eslogs(eslogs) => match eslogs.command {
                EsLogsCommand::Generate {
                    seed,
                    events,
                    table,
                    url,
                } => block_on(subcommand::bench_eslogs_generate(seed, events, table, url)),
                EsLogsCommand::BuildSearchIndex { table, index, url } => block_on(
                    subcommand::bench_eslogs_build_search_index(table, index, url),
                ),
                EsLogsCommand::QuerySearchIndex {
                    index,
                    query,
                    limit,
                    url,
                } => block_on(subcommand::bench_eslogs_query_search_index(
                    index, query, limit, url,
                )),
                EsLogsCommand::BuildParquetTable { table, url } => {
                    block_on(subcommand::bench_eslogs_build_parquet_table(table, url))
                }
                EsLogsCommand::CountParquetTable { table, url } => {
                    block_on(subcommand::bench_eslogs_count_parquet_table(table, url))
                }
                EsLogsCommand::BuildElasticIndex {
                    table,
                    url,
                    elastic_url,
                } => block_on(subcommand::bench_eslogs_build_elastic_table(
                    url,
                    table,
                    elastic_url,
                )),
                EsLogsCommand::QueryElasticIndex {
                    elastic_url,
                    field,
                    term,
                } => block_on(subcommand::bench_eslogs_query_elastic_table(
                    elastic_url,
                    field,
                    term,
                )),
            },
            Corpus::Hits(hits) => match hits.command {
                HitsCommand::Run {
                    workload,
                    url,
                    full,
                } => block_on(bench_hits::bench_hits(&url, &workload, full)),
            },
        },
    }
}
