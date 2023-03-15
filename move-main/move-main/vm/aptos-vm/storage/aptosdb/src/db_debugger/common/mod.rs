// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    db_options::{ledger_db_column_families, state_merkle_db_column_families},
    LEDGER_DB_NAME, STATE_MERKLE_DB_NAME,
};
use anyhow::Result;
use aptos_types::nibble::{nibble_path::NibblePath, Nibble};
use clap::Parser;
use std::path::{Path, PathBuf};

pub const PAGE_SIZE: usize = 10;

#[derive(Parser)]
pub struct DbDir {
    #[clap(long, parse(from_os_str))]
    db_dir: PathBuf,
}

impl DbDir {
    pub fn open_state_merkle_db(&self) -> Result<aptos_schemadb::DB> {
        aptos_schemadb::DB::open_cf_readonly(
            &aptos_schemadb::Options::default(),
            self.db_dir.join(STATE_MERKLE_DB_NAME).as_path(),
            STATE_MERKLE_DB_NAME,
            state_merkle_db_column_families(),
        )
    }

    pub fn open_ledger_db(&self) -> Result<aptos_schemadb::DB> {
        aptos_schemadb::DB::open_cf_readonly(
            &aptos_schemadb::Options::default(),
            self.db_dir.join(LEDGER_DB_NAME).as_path(),
            LEDGER_DB_NAME,
            ledger_db_column_families(),
        )
    }
}

impl AsRef<Path> for DbDir {
    fn as_ref(&self) -> &Path {
        self.db_dir.as_path()
    }
}

pub fn parse_nibble_path(src: &str) -> Result<NibblePath> {
    src.chars()
        .map(|c| Ok(Nibble::from(u8::from_str_radix(&c.to_string(), 16)?)))
        .collect()
}
