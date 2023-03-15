// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    emitter::MAX_RETRIES,
    transaction_generator::{TransactionExecutor, SEND_AMOUNT},
    EmitJobRequest, EmitModeParams,
};
use anyhow::{anyhow, format_err, Context, Result};
use aptos::common::{types::EncodingType, utils::prompt_yes};
use aptos_crypto::ed25519::{Ed25519PrivateKey, Ed25519PublicKey};
use aptos_logger::info;
use aptos_sdk::{
    transaction_builder::{aptos_stdlib, TransactionFactory},
    types::{
        transaction::{
            authenticator::{AuthenticationKey, AuthenticationKeyPreimage},
            SignedTransaction,
        },
        AccountKey, LocalAccount,
    },
};
use core::{
    cmp::min,
    result::Result::{Err, Ok},
};
use futures::StreamExt;
use rand::{rngs::StdRng, SeedableRng};
use std::{path::Path, sync::atomic::AtomicUsize, time::Instant};

#[derive(Debug)]
pub struct AccountMinter<'t> {
    txn_factory: TransactionFactory,
    rng: StdRng,
    source_account: &'t mut LocalAccount,
}

impl<'t> AccountMinter<'t> {
    pub fn new(
        source_account: &'t mut LocalAccount,
        txn_factory: TransactionFactory,
        rng: StdRng,
    ) -> Self {
        Self {
            source_account,
            txn_factory,
            rng,
        }
    }

    /// workflow of create accounts:
    /// 1. Use given source_account as the money source
    /// 1a. Optionally, and if it is root account, mint balance to that account
    /// 2. load tc account to create seed accounts, one seed account for each endpoint
    /// 3. mint coins from faucet to new created seed accounts
    /// 4. split number of requested accounts into equally size of groups
    /// 5. each seed account take responsibility to create one size of group requested accounts and mint coins to them
    /// example:
    /// requested totally 100 new accounts with 10 endpoints
    /// will create 10 seed accounts, each seed account create 10 new accounts
    pub async fn create_accounts(
        &mut self,
        txn_executor: &dyn TransactionExecutor,
        req: &EmitJobRequest,
        mode_params: &EmitModeParams,
        total_requested_accounts: usize,
    ) -> Result<Vec<LocalAccount>> {
        let mut accounts = vec![];
        let expected_num_seed_accounts = (total_requested_accounts / 50)
            .clamp(1, (total_requested_accounts as f32).sqrt() as usize + 1);
        let num_accounts = total_requested_accounts - accounts.len(); // Only minting extra accounts
        let coins_per_account = (req.expected_max_txns / total_requested_accounts as u64)
            .checked_mul(SEND_AMOUNT + req.expected_gas_per_txn * req.gas_price)
            .unwrap()
            .checked_add(req.max_gas_per_txn * req.gas_price)
            .unwrap(); // extra coins for secure to pay none zero gas price
        let txn_factory = self.txn_factory.clone();
        let expected_children_per_seed_account =
            (num_accounts + expected_num_seed_accounts - 1) / expected_num_seed_accounts;
        let coins_per_seed_account = (expected_children_per_seed_account as u64)
            .checked_mul(
                coins_per_account
                    + req.max_gas_per_txn * req.gas_price * req.init_gas_price_multiplier,
            )
            .unwrap()
            .checked_add(req.max_gas_per_txn * req.gas_price * req.init_gas_price_multiplier)
            .unwrap();
        let coins_for_source = coins_per_seed_account
            .checked_mul(expected_num_seed_accounts as u64)
            .unwrap()
            .checked_add(req.max_gas_per_txn * req.gas_price * req.init_gas_price_multiplier)
            .unwrap();
        info!(
            "Account creation plan created for {} accounts with {} balance each.",
            num_accounts, coins_per_account
        );
        info!(
            "    through {} seed accounts with {} each, each to fund {} accounts",
            expected_num_seed_accounts, coins_per_seed_account, expected_children_per_seed_account,
        );
        info!(
            "    because of expecting {} txns and {} gas at {} gas price for each ",
            req.expected_max_txns, req.expected_gas_per_txn, req.gas_price,
        );

        if req.mint_to_root {
            self.mint_to_root(txn_executor, coins_for_source).await?;
        } else {
            let balance = txn_executor
                .get_account_balance(self.source_account.address())
                .await?;
            info!(
                "Source account {} current balance is {}, needed {} coins",
                self.source_account.address(),
                balance,
                coins_for_source
            );

            if req.prompt_before_spending {
                if !prompt_yes(&format!(
                    "plan will consume in total {} balance, are you sure you want to proceed",
                    coins_for_source
                )) {
                    panic!("Aborting");
                }
            } else {
                let max_allowed = 2 * req
                    .expected_max_txns
                    .checked_mul(req.expected_gas_per_txn * req.gas_price)
                    .unwrap();
                assert!(coins_for_source <= max_allowed,
                    "Estimated total coins needed for load test ({}) are larger than expected_max_txns * expected_gas_per_txn, multiplied by 2 to account for rounding up ({})",
                    coins_for_source,
                    max_allowed,
                );
            }

            if balance < coins_for_source {
                return Err(anyhow!(
                    "Source ({}) doesn't have enough coins, balance {} < needed {}",
                    self.source_account.address(),
                    balance,
                    coins_for_source
                ));
            }
        }

        let start = Instant::now();

        let failed_requests = std::iter::repeat_with(|| AtomicUsize::new(0))
            .take(MAX_RETRIES * 2)
            .collect::<Vec<_>>();

        // Create seed accounts with which we can create actual accounts concurrently. Adding
        // additional fund for paying gas fees later.
        let seed_accounts = self
            .create_and_fund_seed_accounts(
                txn_executor,
                expected_num_seed_accounts,
                coins_per_seed_account,
                mode_params.max_submit_batch_size,
                &failed_requests,
            )
            .await?;
        let actual_num_seed_accounts = seed_accounts.len();
        let num_new_child_accounts =
            (num_accounts + actual_num_seed_accounts - 1) / actual_num_seed_accounts;
        info!(
            "Completed creating {} seed accounts in {}s, each with {} coins, had to retry {:?} transactions",
            seed_accounts.len(),
            start.elapsed().as_secs(),
            coins_per_seed_account,
            failed_requests_to_trimmed_vec(failed_requests),
        );
        info!(
            "Creating additional {} accounts with {} coins each",
            num_accounts, coins_per_account
        );

        let seed_rngs = gen_rng_for_reusable_account(actual_num_seed_accounts);
        let start = Instant::now();
        let failed_requests = std::iter::repeat_with(|| AtomicUsize::new(0))
            .take(MAX_RETRIES * 2)
            .collect::<Vec<_>>();

        // For each seed account, create a future and transfer coins from that seed account to new accounts
        let account_futures = seed_accounts
            .into_iter()
            .enumerate()
            .map(|(i, seed_account)| {
                // Spawn new threads
                create_and_fund_new_accounts(
                    seed_account,
                    num_new_child_accounts,
                    coins_per_account,
                    mode_params.max_submit_batch_size,
                    txn_executor,
                    &txn_factory,
                    req.reuse_accounts,
                    if req.reuse_accounts {
                        seed_rngs[i].clone()
                    } else {
                        StdRng::from_rng(self.rng()).unwrap()
                    },
                    &failed_requests,
                )
            });

        // Each future creates 10 accounts, limit concurrency to 1000.
        let stream = futures::stream::iter(account_futures).buffer_unordered(CREATION_PARALLELISM);
        // wait for all futures to complete
        let mut created_accounts = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .map_err(|e| format_err!("Failed to create accounts: {:?}", e))?
            .into_iter()
            .flatten()
            .collect();

        accounts.append(&mut created_accounts);
        assert!(
            accounts.len() >= num_accounts,
            "Something wrong in create_accounts, wanted to create {}, only have {}",
            total_requested_accounts,
            accounts.len()
        );
        info!(
            "Successfully completed creating {} accounts in {}s, had to retry {:?} transactions",
            actual_num_seed_accounts * num_new_child_accounts,
            start.elapsed().as_secs(),
            failed_requests_to_trimmed_vec(failed_requests),
        );
        Ok(accounts)
    }

    pub async fn mint_to_root(
        &mut self,
        txn_executor: &dyn TransactionExecutor,
        amount: u64,
    ) -> Result<()> {
        info!("Minting new coins to root");

        let txn = self
            .source_account
            .sign_with_transaction_builder(self.txn_factory.payload(
                aptos_stdlib::aptos_coin_mint(self.source_account.address(), amount),
            ));
        txn_executor.execute_transactions(&[txn]).await?;
        Ok(())
    }

    pub async fn create_and_fund_seed_accounts(
        &mut self,
        txn_executor: &dyn TransactionExecutor,
        seed_account_num: usize,
        coins_per_seed_account: u64,
        max_submit_batch_size: usize,
        failed_requests: &[AtomicUsize],
    ) -> Result<Vec<LocalAccount>> {
        info!("Creating and funding seeds accounts");
        let mut i = 0;
        let mut seed_accounts = vec![];
        while i < seed_account_num {
            let batch_size = min(max_submit_batch_size, seed_account_num - i);
            let mut rng = StdRng::from_rng(self.rng()).unwrap();
            let mut batch = gen_random_accounts(batch_size, &mut rng);
            let source_account = &mut self.source_account;
            let txn_factory = &self.txn_factory;
            let create_requests: Vec<_> = batch
                .iter()
                .map(|account| {
                    create_and_fund_account_request(
                        source_account,
                        coins_per_seed_account,
                        account.public_key(),
                        txn_factory,
                    )
                })
                .collect();
            txn_executor
                .execute_transactions_with_counter(&create_requests, failed_requests)
                .await?;

            i += batch_size;
            seed_accounts.append(&mut batch);
        }

        Ok(seed_accounts)
    }

    pub async fn load_vasp_account(
        &self,
        txn_executor: &dyn TransactionExecutor,
        index: usize,
    ) -> Result<LocalAccount> {
        let file = "vasp".to_owned() + index.to_string().as_str() + ".key";
        let mint_key: Ed25519PrivateKey = EncodingType::BCS
            .load_key("vasp private key", Path::new(&file))
            .unwrap();
        let account_key = AccountKey::from_private_key(mint_key);
        let address = account_key.authentication_key().derived_address();
        let sequence_number = txn_executor
            .query_sequence_number(address)
            .await
            .map_err(|e| {
                format_err!(
                    "query_sequence_number for account {} failed: {:?}",
                    index,
                    e
                )
            })?;
        Ok(LocalAccount::new(address, account_key, sequence_number))
    }

    pub fn rng(&mut self) -> &mut StdRng {
        &mut self.rng
    }
}

fn failed_requests_to_trimmed_vec(failed_requests: Vec<AtomicUsize>) -> Vec<usize> {
    let mut result = failed_requests
        .into_iter()
        .map(|c| c.into_inner())
        .collect::<Vec<_>>();
    while result.len() > 1 && *result.last().unwrap() == 0 {
        result.pop();
    }
    result
}

fn gen_rng_for_reusable_account(count: usize) -> Vec<StdRng> {
    // use same seed for reuse account creation and reuse
    // TODO: Investigate why we use the same seed and then consider changing
    // this so that we don't do this, since it causes conflicts between
    // runs of the emitter.
    let mut seed = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0,
        0, 0,
    ];
    let mut rngs = vec![];
    for i in 0..count {
        seed[31] = i as u8;
        rngs.push(StdRng::from_seed(seed));
    }
    rngs
}

/// Create `num_new_accounts` by transferring coins from `source_account`. Return Vec of created
/// accounts
async fn create_and_fund_new_accounts<R>(
    mut source_account: LocalAccount,
    num_new_accounts: usize,
    coins_per_new_account: u64,
    max_num_accounts_per_batch: usize,
    txn_executor: &dyn TransactionExecutor,
    txn_factory: &TransactionFactory,
    reuse_account: bool,
    mut rng: R,
    failed_requests: &[AtomicUsize],
) -> Result<Vec<LocalAccount>>
where
    R: ::rand_core::RngCore + ::rand_core::CryptoRng,
{
    let mut i = 0;
    let mut accounts = vec![];

    while i < num_new_accounts {
        let batch_size = min(max_num_accounts_per_batch, num_new_accounts - i);
        let mut batch = if reuse_account {
            info!("Loading {} accounts if they exist", batch_size);
            gen_reusable_accounts(txn_executor, batch_size, &mut rng).await?
        } else {
            let batch = gen_random_accounts(batch_size, &mut rng);
            let creation_requests: Vec<_> = batch
                .as_slice()
                .iter()
                .map(|account| {
                    create_and_fund_account_request(
                        &mut source_account,
                        coins_per_new_account,
                        account.public_key(),
                        txn_factory,
                    )
                })
                .collect();

            txn_executor
                .execute_transactions_with_counter(&creation_requests, failed_requests)
                .await
                .with_context(|| format!("Account {} couldn't mint", source_account.address()))?;

            batch
        };

        i += batch.len();
        accounts.append(&mut batch);
    }
    Ok(accounts)
}

async fn gen_reusable_accounts<R>(
    txn_executor: &dyn TransactionExecutor,
    num_accounts: usize,
    rng: &mut R,
) -> Result<Vec<LocalAccount>>
where
    R: rand_core::RngCore + ::rand_core::CryptoRng,
{
    let mut vasp_accounts = vec![];
    let mut i = 0;
    while i < num_accounts {
        vasp_accounts.push(gen_reusable_account(txn_executor, rng).await?);
        i += 1;
    }
    Ok(vasp_accounts)
}

async fn gen_reusable_account<R>(
    txn_executor: &dyn TransactionExecutor,
    rng: &mut R,
) -> Result<LocalAccount>
where
    R: ::rand_core::RngCore + ::rand_core::CryptoRng,
{
    let account_key = AccountKey::generate(rng);
    let address = account_key.authentication_key().derived_address();
    let sequence_number = txn_executor.query_sequence_number(address).await?;
    Ok(LocalAccount::new(address, account_key, sequence_number))
}

fn gen_random_accounts<R>(num_accounts: usize, rng: &mut R) -> Vec<LocalAccount>
where
    R: ::rand_core::RngCore + ::rand_core::CryptoRng,
{
    (0..num_accounts)
        .map(|_| LocalAccount::generate(rng))
        .collect()
}

pub fn create_and_fund_account_request(
    creation_account: &mut LocalAccount,
    amount: u64,
    pubkey: &Ed25519PublicKey,
    txn_factory: &TransactionFactory,
) -> SignedTransaction {
    let preimage = AuthenticationKeyPreimage::ed25519(pubkey);
    let auth_key = AuthenticationKey::from_preimage(&preimage);
    creation_account.sign_with_transaction_builder(txn_factory.payload(
        aptos_stdlib::aptos_account_transfer(auth_key.derived_address(), amount),
    ))
}

const CREATION_PARALLELISM: usize = 500;
