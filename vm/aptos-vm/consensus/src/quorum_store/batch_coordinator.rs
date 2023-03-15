// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    network::{NetworkSender, QuorumStoreSender},
    quorum_store::{
        batch_aggregator::BatchAggregator,
        batch_store::{BatchStoreCommand, PersistRequest},
        counters,
        proof_coordinator::{ProofCoordinatorCommand, ProofReturnChannel},
        types::{BatchId, Fragment, SerializedTransaction},
    },
};
use aptos_consensus_types::proof_of_store::{LogicalTime, SignedDigestInfo};
use aptos_logger::prelude::*;
use aptos_types::PeerId;
use std::collections::HashMap;
use tokio::sync::{
    mpsc::{Receiver, Sender},
    oneshot,
};

#[derive(Debug)]
pub enum BatchCoordinatorCommand {
    AppendToBatch(Vec<SerializedTransaction>, BatchId),
    EndBatch(
        Vec<SerializedTransaction>,
        BatchId,
        LogicalTime,
        ProofReturnChannel,
    ),
    Shutdown(oneshot::Sender<()>),
    RemoteFragment(Box<Fragment>),
}

pub struct BatchCoordinator {
    epoch: u64,
    my_peer_id: PeerId,
    network_sender: NetworkSender,
    command_rx: Receiver<BatchCoordinatorCommand>,
    batch_store_tx: Sender<BatchStoreCommand>,
    proof_coordinator_tx: Sender<ProofCoordinatorCommand>,
    max_batch_bytes: usize,
    remote_batch_aggregators: HashMap<PeerId, BatchAggregator>,
    local_batch_aggregator: BatchAggregator,
    local_fragment_id: usize,
}

impl BatchCoordinator {
    pub(crate) fn new(
        epoch: u64, //TODO: pass the epoch config
        my_peer_id: PeerId,
        network_sender: NetworkSender,
        wrapper_command_rx: Receiver<BatchCoordinatorCommand>,
        batch_store_tx: Sender<BatchStoreCommand>,
        proof_coordinator_tx: Sender<ProofCoordinatorCommand>,
        max_batch_bytes: usize,
    ) -> Self {
        Self {
            epoch,
            my_peer_id,
            network_sender,
            command_rx: wrapper_command_rx,
            batch_store_tx,
            proof_coordinator_tx,
            max_batch_bytes,
            remote_batch_aggregators: HashMap::new(),
            local_batch_aggregator: BatchAggregator::new(max_batch_bytes),
            local_fragment_id: 0,
        }
    }

    /// Aggregate & compute rolling digest, synchronously by worker.
    fn handle_append_to_batch(
        &mut self,
        fragment_payload: Vec<SerializedTransaction>,
        batch_id: BatchId,
    ) -> Fragment {
        match self.local_batch_aggregator.append_transactions(
            batch_id,
            self.local_fragment_id,
            fragment_payload.clone(),
        ) {
            Ok(()) => Fragment::new(
                self.epoch,
                batch_id,
                self.local_fragment_id,
                fragment_payload,
                None,
                self.my_peer_id,
            ),
            Err(e) => {
                unreachable!(
                    "[QuorumStore] Aggregation failed for own fragments with error {:?}",
                    e
                );
            },
        }
    }

    /// Finalize the batch & digest, synchronously by worker.
    async fn handle_end_batch(
        &mut self,
        fragment_payload: Vec<SerializedTransaction>,
        batch_id: BatchId,
        expiration: LogicalTime,
        proof_tx: ProofReturnChannel,
    ) -> (BatchStoreCommand, Fragment) {
        match self.local_batch_aggregator.end_batch(
            batch_id,
            self.local_fragment_id,
            fragment_payload.clone(),
        ) {
            Ok((num_bytes, payload, digest)) => {
                let fragment = Fragment::new(
                    self.epoch,
                    batch_id,
                    self.local_fragment_id,
                    fragment_payload,
                    Some(expiration),
                    self.my_peer_id,
                );

                self.proof_coordinator_tx
                    .send(ProofCoordinatorCommand::InitProof(
                        SignedDigestInfo::new(
                            digest,
                            expiration,
                            payload.len() as u64,
                            num_bytes as u64,
                        ),
                        fragment.batch_id(),
                        proof_tx,
                    ))
                    .await
                    .expect("Failed to send to ProofBuilder");

                let persist_request = PersistRequest::new(
                    self.my_peer_id,
                    payload.clone(),
                    digest,
                    num_bytes,
                    expiration,
                );
                (BatchStoreCommand::Persist(persist_request), fragment)
            },
            Err(e) => {
                unreachable!(
                    "[QuorumStore] Aggregation failed for own fragments with error {:?}",
                    e
                );
            },
        }
    }

    async fn handle_fragment(&mut self, fragment: Fragment) {
        let source = fragment.source();
        let entry = self
            .remote_batch_aggregators
            .entry(source)
            .or_insert_with(|| BatchAggregator::new(self.max_batch_bytes));
        if let Some(expiration) = fragment.maybe_expiration() {
            counters::DELIVERED_END_BATCH_COUNT.inc();
            // end batch message
            debug!(
                "QS: got end batch message from {:?} batch_id {}, fragment_id {}",
                source,
                fragment.batch_id(),
                fragment.fragment_id(),
            );
            if expiration.epoch() == self.epoch {
                match entry.end_batch(
                    fragment.batch_id(),
                    fragment.fragment_id(),
                    fragment.into_transactions(),
                ) {
                    Ok((num_bytes, payload, digest)) => {
                        let persist_cmd = BatchStoreCommand::Persist(PersistRequest::new(
                            source, payload, digest, num_bytes, expiration,
                        ));
                        self.batch_store_tx
                            .send(persist_cmd)
                            .await
                            .expect("BatchStore receiver not available");
                    },
                    Err(e) => {
                        debug!("Could not append batch from {:?}, error {:?}", source, e);
                    },
                }
            }
            // Malformed request with an inconsistent expiry epoch.
            else {
                debug!(
                    "QS: got end batch message epoch {} {}",
                    expiration.epoch(),
                    self.epoch
                );
            }
        } else if let Err(e) = entry.append_transactions(
            fragment.batch_id(),
            fragment.fragment_id(),
            fragment.into_transactions(),
        ) {
            debug!("Could not append batch from {:?}, error {:?}", source, e);
        }
    }

    pub(crate) async fn start(mut self) {
        while let Some(command) = self.command_rx.recv().await {
            match command {
                BatchCoordinatorCommand::Shutdown(ack_tx) => {
                    ack_tx
                        .send(())
                        .expect("Failed to send shutdown ack to QuorumStoreCoordinator");
                    break;
                },
                BatchCoordinatorCommand::AppendToBatch(fragment_payload, batch_id) => {
                    debug!("QS: end batch cmd received, batch id {}", batch_id);
                    let msg = self.handle_append_to_batch(fragment_payload, batch_id);
                    self.network_sender.broadcast_fragment(msg).await;

                    self.local_fragment_id += 1;
                },
                BatchCoordinatorCommand::EndBatch(
                    fragment_payload,
                    batch_id,
                    logical_time,
                    proof_tx,
                ) => {
                    debug!("QS: end batch cmd received, batch id = {}", batch_id);
                    let (batch_store_command, fragment) = self
                        .handle_end_batch(fragment_payload, batch_id, logical_time, proof_tx)
                        .await;

                    self.network_sender.broadcast_fragment(fragment).await;

                    self.batch_store_tx
                        .send(batch_store_command)
                        .await
                        .expect("Failed to send to BatchStore");

                    counters::NUM_FRAGMENT_PER_BATCH.observe(self.local_fragment_id as f64);

                    self.local_fragment_id = 0;
                },
                BatchCoordinatorCommand::RemoteFragment(fragment) => {
                    self.handle_fragment(*fragment).await;
                },
            }
        }
    }
}
