// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    error::DbError,
    quorum_store::{
        schema::{BatchIdSchema, BatchSchema, BATCH_CF_NAME, BATCH_ID_CF_NAME},
        types::{BatchId, PersistedValue},
    },
};
use anyhow::Result;
use aptos_crypto::HashValue;
use aptos_logger::{debug, info};
use aptos_schemadb::{Options, ReadOptions, SchemaBatch, DB};
use std::{collections::HashMap, path::Path, time::Instant};

/// The name of the quorum store db file
pub const QUORUM_STORE_DB_NAME: &str = "quorumstoreDB";

pub trait BatchIdDB: Send + Sync {
    fn clean_and_get_batch_id(&self, current_epoch: u64) -> Result<Option<BatchId>, DbError>;
    fn save_batch_id(&self, epoch: u64, batch_id: BatchId) -> Result<(), DbError>;
}

pub struct QuorumStoreDB {
    db: DB,
}

impl QuorumStoreDB {
    pub(crate) fn new<P: AsRef<Path> + Clone>(db_root_path: P) -> Self {
        let column_families = vec![BATCH_CF_NAME, BATCH_ID_CF_NAME];

        // TODO: this fails twins tests because it assumes a unique path per process
        let path = db_root_path.as_ref().join(QUORUM_STORE_DB_NAME);
        let instant = Instant::now();
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let db = DB::open(path.clone(), QUORUM_STORE_DB_NAME, column_families, &opts)
            .expect("QuorumstoreDB open failed; unable to continue");

        info!(
            "Opened QuorumstoreDB at {:?} in {} ms",
            path,
            instant.elapsed().as_millis()
        );

        Self { db }
    }

    pub(crate) fn delete_batches(&self, digests: Vec<HashValue>) -> Result<(), DbError> {
        let batch = SchemaBatch::new();
        for digest in digests.iter() {
            debug!("QS: db delete digest {}", digest);
            batch.delete::<BatchSchema>(digest)?;
        }
        self.db.write_schemas(batch)?;
        Ok(())
    }

    pub(crate) fn get_all_batches(&self) -> Result<HashMap<HashValue, PersistedValue>> {
        let mut iter = self.db.iter::<BatchSchema>(ReadOptions::default())?;
        iter.seek_to_first();
        iter.collect::<Result<HashMap<HashValue, PersistedValue>>>()
    }

    pub(crate) fn save_batch(
        &self,
        digest: HashValue,
        batch: PersistedValue,
    ) -> Result<(), DbError> {
        debug!(
            "QS: db persists digest {} expiration {:?}",
            digest, batch.expiration
        );
        Ok(self.db.put::<BatchSchema>(&digest, &batch)?)
    }

    pub(crate) fn get_batch(&self, digest: HashValue) -> Result<Option<PersistedValue>, DbError> {
        Ok(self.db.get::<BatchSchema>(&digest)?)
    }

    fn delete_batch_id(&self, epoch: u64) -> Result<(), DbError> {
        let batch = SchemaBatch::new();
        batch.delete::<BatchIdSchema>(&epoch)?;
        self.db.write_schemas(batch)?;
        Ok(())
    }
}

impl BatchIdDB for QuorumStoreDB {
    fn clean_and_get_batch_id(&self, current_epoch: u64) -> Result<Option<BatchId>, DbError> {
        let mut iter = self.db.iter::<BatchIdSchema>(ReadOptions::default())?;
        iter.seek_to_first();
        let epoch_batch_id = iter.collect::<Result<HashMap<u64, BatchId>>>()?;
        let mut ret = None;
        for (epoch, batch_id) in epoch_batch_id {
            assert!(current_epoch >= epoch);
            if epoch < current_epoch {
                self.delete_batch_id(epoch)
                    .expect("Could not delete from db");
            } else {
                ret = Some(batch_id);
            }
        }
        Ok(ret)
    }

    fn save_batch_id(&self, epoch: u64, batch_id: BatchId) -> Result<(), DbError> {
        Ok(self.db.put::<BatchIdSchema>(&epoch, &batch_id)?)
    }
}
