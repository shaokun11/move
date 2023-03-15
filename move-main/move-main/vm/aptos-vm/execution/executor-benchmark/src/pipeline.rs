// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    benchmark_transaction::BenchmarkTransaction, TransactionCommitter, TransactionExecutor,
};
use aptos_executor::block_executor::{BlockExecutor, TransactionBlockExecutor};
use aptos_executor_types::BlockExecutorTrait;
use aptos_logger::info;
use aptos_types::transaction::Version;
use std::{
    marker::PhantomData,
    sync::{mpsc, Arc},
    thread::JoinHandle,
};

pub struct Pipeline<V> {
    join_handles: Vec<JoinHandle<()>>,
    phantom: PhantomData<V>,
}

impl<V> Pipeline<V>
where
    V: TransactionBlockExecutor<BenchmarkTransaction> + 'static,
{
    pub fn new(
        executor: BlockExecutor<V, BenchmarkTransaction>,
        version: Version,
    ) -> (Self, mpsc::SyncSender<Vec<BenchmarkTransaction>>) {
        let parent_block_id = executor.committed_block_id();
        let executor_1 = Arc::new(executor);
        let executor_2 = executor_1.clone();

        let (block_sender, block_receiver) =
            mpsc::sync_channel::<Vec<BenchmarkTransaction>>(50 /* bound */);
        let (commit_sender, commit_receiver) = mpsc::sync_channel(3 /* bound */);

        let exe_thread = std::thread::Builder::new()
            .name("txn_executor".to_string())
            .spawn(move || {
                let mut exe = TransactionExecutor::new(
                    executor_1,
                    parent_block_id,
                    version,
                    Some(commit_sender),
                );
                while let Ok(transactions) = block_receiver.recv() {
                    info!("Received block of size {:?} to execute", transactions.len());
                    exe.execute_block(transactions);
                }
            })
            .expect("Failed to spawn transaction executor thread.");
        let commit_thread = std::thread::Builder::new()
            .name("txn_committer".to_string())
            .spawn(move || {
                let mut committer = TransactionCommitter::new(executor_2, version, commit_receiver);
                committer.run();
            })
            .expect("Failed to spawn transaction committer thread.");
        let join_handles = vec![exe_thread, commit_thread];

        (
            Self {
                join_handles,
                phantom: PhantomData,
            },
            block_sender,
        )
    }

    pub fn join(self) {
        for handle in self.join_handles {
            handle.join().unwrap()
        }
    }
}
