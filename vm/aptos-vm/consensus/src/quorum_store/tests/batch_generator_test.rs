// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    error::DbError,
    quorum_store::{
        batch_coordinator::BatchCoordinatorCommand,
        batch_generator::BatchGenerator,
        quorum_store_db::BatchIdDB,
        tests::utils::{create_vec_serialized_transactions, create_vec_signed_transactions},
        types::{BatchId, SerializedTransaction},
    },
    test_utils::build_empty_tree,
};
use aptos_config::config::QuorumStoreConfig;
use aptos_consensus_types::common::TransactionSummary;
use aptos_mempool::{QuorumStoreRequest, QuorumStoreResponse};
use aptos_types::transaction::SignedTransaction;
use futures::{
    channel::mpsc::{channel, Receiver},
    StreamExt,
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc::channel as TokioChannel, time::timeout};

pub struct MockBatchIdDB {}

impl MockBatchIdDB {
    pub fn new() -> Self {
        Self {}
    }
}

impl BatchIdDB for MockBatchIdDB {
    // The first batch will be index 1
    fn clean_and_get_batch_id(
        &self,
        _current_epoch: u64,
    ) -> anyhow::Result<Option<BatchId>, DbError> {
        Ok(Some(BatchId::new_for_test(0)))
    }

    fn save_batch_id(&self, _epoch: u64, _batch_id: BatchId) -> anyhow::Result<(), DbError> {
        Ok(())
    }
}

async fn queue_mempool_batch_response(
    txns: Vec<SignedTransaction>,
    quorum_store_to_mempool_receiver: &mut Receiver<QuorumStoreRequest>,
) -> Vec<TransactionSummary> {
    if let QuorumStoreRequest::GetBatchRequest(
        _max_batch_size,
        _max_bytes,
        exclude_txns,
        callback,
    ) = timeout(
        Duration::from_millis(1_000),
        quorum_store_to_mempool_receiver.select_next_some(),
    )
    .await
    .unwrap()
    {
        callback
            .send(Ok(QuorumStoreResponse::GetBatchResponse(txns)))
            .unwrap();
        exclude_txns
    } else {
        panic!("Unexpected variant")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_creation() {
    let (quorum_store_to_mempool_tx, mut quorum_store_to_mempool_rx) = channel(1_024);
    let (batch_coordinator_cmd_tx, mut batch_coordinator_cmd_rx) = TokioChannel(100);

    let txn_size = 168;
    create_vec_serialized_transactions(50)
        .iter()
        .for_each(|txn| assert_eq!(txn_size, txn.len()));

    let config = QuorumStoreConfig {
        max_batch_bytes: 9 * txn_size,
        ..Default::default()
    };

    let block_store = build_empty_tree();

    let mut batch_generator = BatchGenerator::new(
        0,
        Arc::new(MockBatchIdDB::new()),
        quorum_store_to_mempool_tx,
        batch_coordinator_cmd_tx,
        1000,
        config.mempool_txn_pull_max_count,
        config.mempool_txn_pull_max_bytes,
        config.max_batch_counts,
        config.max_batch_bytes,
        config.batch_expiry_round_gap_when_init,
        config.end_batch_ms,
        block_store,
    );

    let serialize = |signed_txns: &Vec<SignedTransaction>| -> Vec<SerializedTransaction> {
        signed_txns
            .iter()
            .map(SerializedTransaction::from_signed_txn)
            .collect()
    };

    let join_handle = tokio::spawn(async move {
        let mut num_txns = 0;

        let signed_txns = create_vec_signed_transactions(1);
        queue_mempool_batch_response(
            vec![signed_txns[0].clone()],
            &mut quorum_store_to_mempool_rx,
        )
        .await;
        // Expect AppendToBatch for 1 txn
        let quorum_store_command = batch_coordinator_cmd_rx.recv().await.unwrap();
        if let BatchCoordinatorCommand::AppendToBatch(data, batch_id) = quorum_store_command {
            assert_eq!(batch_id, BatchId::new_for_test(1));
            assert_eq!(data.len(), signed_txns.len());
            assert_eq!(data, serialize(&signed_txns));
        } else {
            panic!("Unexpected variant")
        }
        num_txns += 1;

        let signed_txns = create_vec_signed_transactions(9);
        // Expect single exclude_txn
        let exclude_txns =
            queue_mempool_batch_response(signed_txns.clone(), &mut quorum_store_to_mempool_rx)
                .await;
        assert_eq!(exclude_txns.len(), num_txns);
        // Expect EndBatch for 1 + 9 = 10 txns. The last txn pulled is not included in the batch.
        let quorum_store_command = batch_coordinator_cmd_rx.recv().await.unwrap();
        if let BatchCoordinatorCommand::EndBatch(data, _, _, _) = quorum_store_command {
            assert_eq!(data.len(), signed_txns.len() - 1);
            assert_eq!(data, serialize(&signed_txns[0..8].to_vec()));
        } else {
            panic!("Unexpected variant")
        }
        num_txns += 8;

        let signed_txns = create_vec_signed_transactions(9);
        // Expect 1 + 8 = 9 exclude_txn
        let exclude_txns =
            queue_mempool_batch_response(signed_txns.clone(), &mut quorum_store_to_mempool_rx)
                .await;
        assert_eq!(exclude_txns.len(), num_txns);
        // Expect AppendBatch for 9 txns
        let quorum_store_command = batch_coordinator_cmd_rx.recv().await.unwrap();
        if let BatchCoordinatorCommand::AppendToBatch(data, batch_id) = quorum_store_command {
            assert_eq!(batch_id, BatchId::new_for_test(2));
            assert_eq!(data.len(), signed_txns.len());
            assert_eq!(data, serialize(&signed_txns));
        } else {
            panic!("Unexpected variant")
        }
    });

    let result = batch_generator.handle_scheduled_pull(false).await;
    assert!(result.is_none());
    let result = batch_generator.handle_scheduled_pull(false).await;
    assert!(result.is_some());
    let result = batch_generator.handle_scheduled_pull(false).await;
    assert!(result.is_none());

    timeout(Duration::from_millis(10_000), join_handle)
        .await
        .unwrap()
        .unwrap();
}
