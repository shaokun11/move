// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    args::{ClusterArgs, EmitArgs},
    cluster::Cluster,
    emitter::{stats::TxnStats, EmitJobMode, EmitJobRequest, TxnEmitter},
    instance::Instance,
    EntryPoints, TransactionType, TransactionTypeArg,
};
use anyhow::{Context, Result};
use aptos_sdk::transaction_builder::TransactionFactory;
use rand::{rngs::StdRng, SeedableRng};
use std::time::Duration;

pub async fn emit_transactions(
    cluster_args: &ClusterArgs,
    emit_args: &EmitArgs,
) -> Result<TxnStats> {
    let cluster = Cluster::try_from_cluster_args(cluster_args)
        .await
        .context("Failed to build cluster")?;
    emit_transactions_with_cluster(&cluster, emit_args, cluster_args.reuse_accounts).await
}

pub async fn emit_transactions_with_cluster(
    cluster: &Cluster,
    args: &EmitArgs,
    reuse_accounts: bool,
) -> Result<TxnStats> {
    let emitter_mode = EmitJobMode::create(args.mempool_backlog, args.target_tps);

    let duration = Duration::from_secs(args.duration);
    let client = cluster.random_instance().rest_client();
    let mut coin_source_account = cluster.load_coin_source_account(&client).await?;
    let emitter = TxnEmitter::new(
        TransactionFactory::new(cluster.chain_id)
            .with_transaction_expiration_time(args.txn_expiration_time_secs)
            .with_gas_unit_price(aptos_global_constants::GAS_UNIT_PRICE),
        StdRng::from_entropy(),
    );

    let arg_transaction_types = args
        .transaction_type
        .iter()
        .map(|t| match t {
            TransactionTypeArg::CoinTransfer => TransactionType::CoinTransfer {
                invalid_transaction_ratio: args.invalid_tx,
                sender_use_account_pool: false,
            },
            TransactionTypeArg::AccountGeneration => TransactionType::default_account_generation(),
            TransactionTypeArg::AccountGenerationLargePool => TransactionType::AccountGeneration {
                add_created_accounts_to_pool: true,
                max_account_working_set: 50_000_000,
                creation_balance: 200_000_000,
            },
            TransactionTypeArg::NftMintAndTransfer => TransactionType::NftMintAndTransfer,
            TransactionTypeArg::PublishPackage => TransactionType::PublishPackage {
                use_account_pool: false,
            },
            TransactionTypeArg::CustomFunctionLargeModuleWorkingSet => {
                TransactionType::CallCustomModules {
                    entry_point: EntryPoints::Nop,
                    num_modules: 1000,
                    use_account_pool: false,
                }
            },
            TransactionTypeArg::CreateNewResource => TransactionType::CallCustomModules {
                entry_point: EntryPoints::BytesMakeOrChange {
                    data_length: Some(32),
                },
                num_modules: 1,
                use_account_pool: true,
            },
            TransactionTypeArg::NoOp => TransactionType::CallCustomModules {
                entry_point: EntryPoints::Nop,
                num_modules: 1,
                use_account_pool: false,
            },
        })
        .collect::<Vec<_>>();

    let arg_transaction_weights = if args.transaction_weights.is_empty() {
        vec![1; arg_transaction_types.len()]
    } else {
        assert_eq!(
            args.transaction_weights.len(),
            arg_transaction_types.len(),
            "Transaction types and weights need to be the same length"
        );
        args.transaction_weights.clone()
    };
    let arg_transaction_phases = if args.transaction_phases.is_empty() {
        vec![0; arg_transaction_types.len()]
    } else {
        assert_eq!(
            args.transaction_phases.len(),
            arg_transaction_types.len(),
            "Transaction types and phases need to be the same length"
        );
        args.transaction_phases.clone()
    };

    let mut transaction_mix_per_phase: Vec<Vec<(TransactionType, usize)>> = Vec::new();
    for (transaction_type, (weight, phase)) in arg_transaction_types.into_iter().zip(
        arg_transaction_weights
            .into_iter()
            .zip(arg_transaction_phases.into_iter()),
    ) {
        assert!(
            phase <= transaction_mix_per_phase.len(),
            "cannot skip phases ({})",
            transaction_mix_per_phase.len()
        );
        if phase == transaction_mix_per_phase.len() {
            transaction_mix_per_phase.push(Vec::new());
        }
        transaction_mix_per_phase
            .get_mut(phase)
            .unwrap()
            .push((transaction_type, weight));
    }

    let mut emit_job_request =
        EmitJobRequest::new(cluster.all_instances().map(Instance::rest_client).collect())
            .mode(emitter_mode)
            .transaction_mix_per_phase(transaction_mix_per_phase)
            .txn_expiration_time_secs(args.txn_expiration_time_secs)
            .delay_after_minting(Duration::from_secs(args.delay_after_minting.unwrap_or(0)));
    if reuse_accounts {
        emit_job_request = emit_job_request.reuse_accounts();
    }
    if let Some(max_transactions_per_account) = args.max_transactions_per_account {
        emit_job_request =
            emit_job_request.max_transactions_per_account(max_transactions_per_account);
    }

    if let Some(gas_price) = args.gas_price {
        emit_job_request = emit_job_request.gas_price(gas_price);
    }

    if let Some(max_gas_per_txn) = args.max_gas_per_txn {
        emit_job_request = emit_job_request.max_gas_per_txn(max_gas_per_txn);
    }

    if let Some(init_gas_price_multiplier) = args.init_gas_price_multiplier {
        emit_job_request = emit_job_request.init_gas_price_multiplier(init_gas_price_multiplier);
    }

    if let Some(expected_max_txns) = args.expected_max_txns {
        emit_job_request = emit_job_request.expected_max_txns(expected_max_txns);
    }
    if let Some(expected_gas_per_txn) = args.expected_gas_per_txn {
        emit_job_request = emit_job_request.expected_gas_per_txn(expected_gas_per_txn);
    }
    if !cluster.coin_source_is_root {
        emit_job_request = emit_job_request.prompt_before_spending();
    }
    let stats = emitter
        .emit_txn_for_with_stats(
            &mut coin_source_account,
            emit_job_request,
            duration,
            (args.duration / 10).clamp(1, 10),
        )
        .await?;
    Ok(stats)
}
