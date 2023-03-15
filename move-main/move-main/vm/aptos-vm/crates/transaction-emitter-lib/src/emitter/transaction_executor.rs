// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use super::RETRY_POLICY;
use crate::transaction_generator::TransactionExecutor;
use anyhow::Result;
use aptos_logger::{sample, sample::SampleRate, warn};
use aptos_rest_client::Client as RestClient;
use aptos_sdk::{
    move_types::account_address::AccountAddress, types::transaction::SignedTransaction,
};
use async_trait::async_trait;
use futures::future::join_all;
use rand::{rngs::StdRng, seq::SliceRandom, thread_rng, Rng, SeedableRng};
use std::{sync::atomic::AtomicUsize, time::Duration};

// Reliable/retrying transaction executor, used for initializing
pub struct RestApiTransactionExecutor {
    pub rest_clients: Vec<RestClient>,
    pub max_retries: usize,
}

impl RestApiTransactionExecutor {
    fn random_rest_client(&self) -> &RestClient {
        let mut rng = thread_rng();
        self.rest_clients.choose(&mut rng).unwrap()
    }

    fn random_rest_client_from_rng<R>(&self, rng: &mut R) -> &RestClient
    where
        R: Rng + ?Sized,
    {
        self.rest_clients.choose(rng).unwrap()
    }

    async fn submit_check_and_retry(
        &self,
        txn: &SignedTransaction,
        failure_counter: &[AtomicUsize],
        run_seed: u64,
    ) -> Result<()> {
        for i in 0..self.max_retries {
            // All transactions from the same sender, need to be submitted to the same client
            // in the same retry round, so that they are not placed in parking lot.
            // Do so by selecting a client via seeded random selection.
            let seed = [
                i.to_le_bytes().to_vec(),
                run_seed.to_le_bytes().to_vec(),
                txn.sender().to_vec(),
            ]
            .concat();
            let mut seeded_rng = StdRng::from_seed(*aptos_crypto::HashValue::sha3_256_of(&seed));
            let rest_client = self.random_rest_client_from_rng(&mut seeded_rng);
            if submit_and_check(
                rest_client,
                txn,
                &failure_counter[(i * 2).min(failure_counter.len() - 1)],
                &failure_counter[(i * 2 + 1).min(failure_counter.len() - 1)],
            )
            .await
            .is_ok()
            {
                return Ok(());
            };
        }

        // if submission timeouts, it might still get committed:
        self.random_rest_client()
            .wait_for_signed_transaction_bcs(txn)
            .await?;

        Ok(())
    }
}

async fn submit_and_check(
    rest_client: &RestClient,
    txn: &SignedTransaction,
    submit_failure_counter: &AtomicUsize,
    wait_failure_counter: &AtomicUsize,
) -> Result<()> {
    if let Err(err) = rest_client.submit_bcs(txn).await {
        sample!(
            SampleRate::Duration(Duration::from_secs(60)),
            warn!(
                "[{}] Failed submitting transaction: {}",
                rest_client.path_prefix_string(),
                err,
            )
        );
        submit_failure_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // even if txn fails submitting, it might get committed, so wait to see if that is the case.
    }
    if let Err(err) = rest_client
        .wait_for_transaction_by_hash(
            txn.clone().committed_hash(),
            txn.expiration_timestamp_secs(),
            None,
            Some(Duration::from_secs(10)),
        )
        .await
    {
        sample!(
            SampleRate::Duration(Duration::from_secs(60)),
            warn!(
                "[{}] Failed waiting on a transaction: {}",
                rest_client.path_prefix_string(),
                err,
            )
        );
        wait_failure_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(err)?;
    }
    Ok(())
}

#[async_trait]
impl TransactionExecutor for RestApiTransactionExecutor {
    async fn get_account_balance(&self, account_address: AccountAddress) -> Result<u64> {
        Ok(RETRY_POLICY
            .retry(move || {
                self.random_rest_client()
                    .get_account_balance(account_address)
            })
            .await?
            .into_inner()
            .get())
    }

    async fn query_sequence_number(&self, account_address: AccountAddress) -> Result<u64> {
        Ok(RETRY_POLICY
            .retry(move || self.random_rest_client().get_account_bcs(account_address))
            .await?
            .into_inner()
            .sequence_number())
    }

    async fn execute_transactions(&self, txns: &[SignedTransaction]) -> Result<()> {
        self.execute_transactions_with_counter(txns, &[AtomicUsize::new(0)])
            .await
    }

    async fn execute_transactions_with_counter(
        &self,
        txns: &[SignedTransaction],
        failure_counter: &[AtomicUsize],
    ) -> Result<()> {
        let run_seed: u64 = thread_rng().gen();

        join_all(
            txns.iter()
                .map(|txn| self.submit_check_and_retry(txn, failure_counter, run_seed)),
        )
        .await
        .into_iter()
        .collect::<Result<Vec<()>, anyhow::Error>>()?;

        Ok(())
    }
}
