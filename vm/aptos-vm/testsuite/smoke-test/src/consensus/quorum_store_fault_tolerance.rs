// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    aptos_cli::validator::generate_blob, smoke_test_environment::SwarmBuilder,
    txn_emitter::generate_traffic,
};
use aptos_consensus::QUORUM_STORE_DB_NAME;
use aptos_forge::{NodeExt, Swarm, SwarmExt, TransactionType};
use aptos_logger::info;
use aptos_types::{
    on_chain_config::{ConsensusConfigV1, OnChainConsensusConfig},
    PeerId,
};
use move_core_types::language_storage::CORE_CODE_ADDRESS;
use std::{fs, sync::Arc, time::Duration};

const MAX_WAIT_SECS: u64 = 60;

async fn generate_traffic_and_assert_committed(swarm: &mut dyn Swarm, nodes: &[PeerId]) {
    let rest_client = swarm.validator(nodes[0]).unwrap().rest_client();

    // faucet can make our root LocalAccount sequence number get out of sync.
    swarm
        .chain_info()
        .resync_root_account_seq_num(&rest_client)
        .await
        .unwrap();

    let txn_stat = generate_traffic(swarm, nodes, Duration::from_secs(20), 1, vec![vec![
        (
            TransactionType::CoinTransfer {
                invalid_transaction_ratio: 0,
                sender_use_account_pool: false,
            },
            70,
        ),
        (
            TransactionType::AccountGeneration {
                add_created_accounts_to_pool: true,
                max_account_working_set: 1_000_000,
                creation_balance: 1_000_000,
            },
            20,
        ),
    ]])
    .await
    .unwrap();
    println!("{:?}", txn_stat.rate());
    // assert some much smaller number than expected, so it doesn't fail under contention
    assert!(txn_stat.submitted > 30);
    assert!(txn_stat.committed > 30);
}

// TODO: remove when quorum store becomes the in-code default
#[tokio::test]
async fn test_onchain_config_quorum_store_enabled() {
    let (mut swarm, mut cli, _faucet) = SwarmBuilder::new_local(4)
        .with_aptos()
        // Start with V1
        .with_init_genesis_config(Arc::new(|genesis_config| {
            genesis_config.consensus_config =
                OnChainConsensusConfig::V1(ConsensusConfigV1::default())
        }))
        .build_with_cli(0)
        .await;
    let validator_peer_ids = swarm.validators().map(|v| v.peer_id()).collect::<Vec<_>>();

    generate_traffic_and_assert_committed(&mut swarm, &validator_peer_ids).await;

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();

    let root_cli_index = cli.add_account_with_address_to_cli(
        swarm.root_key(),
        swarm.chain_info().root_account().address(),
    );

    let rest_client = swarm.validators().next().unwrap().rest_client();
    let current_consensus_config: OnChainConsensusConfig = bcs::from_bytes(
        &rest_client
            .get_account_resource_bcs::<Vec<u8>>(
                CORE_CODE_ADDRESS,
                "0x1::consensus_config::ConsensusConfig",
            )
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();

    let inner = match current_consensus_config {
        OnChainConsensusConfig::V1(inner) => inner,
        OnChainConsensusConfig::V2(_) => panic!("Unexpected V2 config"),
    };

    // Change to V2
    let new_consensus_config = OnChainConsensusConfig::V2(ConsensusConfigV1 { ..inner });

    let update_consensus_config_script = format!(
        r#"
    script {{
        use aptos_framework::aptos_governance;
        use aptos_framework::consensus_config;
        fun main(core_resources: &signer) {{
            let framework_signer = aptos_governance::get_signer_testnet_only(core_resources, @0000000000000000000000000000000000000000000000000000000000000001);
            let config_bytes = {};
            consensus_config::set(&framework_signer, config_bytes);
        }}
    }}
    "#,
        generate_blob(&bcs::to_bytes(&new_consensus_config).unwrap())
    );
    cli.run_script(root_cli_index, &update_consensus_config_script)
        .await
        .unwrap();

    generate_traffic_and_assert_committed(&mut swarm, &validator_peer_ids).await;
}

/// Checks progress even if half the nodes are not actually writing batches to the DB.
/// Shows that remote reading of batches is working.
/// Note this is more than expected (f) byzantine behavior.
#[tokio::test]
async fn test_remote_batch_reads() {
    let mut swarm = SwarmBuilder::new_local(4)
        .with_aptos()
        .with_init_config(Arc::new(|_, conf, _| {
            conf.api.failpoints_enabled = true;
        }))
        // TODO: remove when quorum store becomes the in-code default
        .with_init_genesis_config(Arc::new(|genesis_config| {
            genesis_config.consensus_config =
                OnChainConsensusConfig::V2(ConsensusConfigV1::default())
        }))
        .build()
        .await;
    let validator_peer_ids = swarm.validators().map(|v| v.peer_id()).collect::<Vec<_>>();

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();

    for peer_id in validator_peer_ids.iter().take(2) {
        let validator_client = swarm.validator(*peer_id).unwrap().rest_client();
        validator_client
            .set_failpoint("quorum_store::save".to_string(), "return".to_string())
            .await
            .unwrap();
    }

    generate_traffic_and_assert_committed(&mut swarm, &[validator_peer_ids[2]]).await;

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();
}

async fn test_batch_id_on_restart(do_wipe_db: bool) {
    let mut swarm = SwarmBuilder::new_local(4)
        .with_aptos()
        // TODO: remove when quorum store becomes the in-code default
        .with_init_genesis_config(Arc::new(|genesis_config| {
            genesis_config.consensus_config =
                OnChainConsensusConfig::V2(ConsensusConfigV1::default())
        }))
        .build()
        .await;
    let validator_peer_ids = swarm.validators().map(|v| v.peer_id()).collect::<Vec<_>>();
    let node_to_restart = validator_peer_ids[0];

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();

    generate_traffic_and_assert_committed(&mut swarm, &[node_to_restart]).await;

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();

    info!("restart node 0, db intact");
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .restart()
        .await
        .unwrap();

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();

    generate_traffic_and_assert_committed(&mut swarm, &[node_to_restart]).await;

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();

    info!("stop node 0");
    swarm.validator_mut(node_to_restart).unwrap().stop();
    if do_wipe_db {
        info!("wipe only quorum store db");
        let node0_config = swarm.validator(node_to_restart).unwrap().config().clone();
        let db_dir = node0_config.storage.dir();
        let quorum_store_db_dir = db_dir.join(QUORUM_STORE_DB_NAME);
        fs::remove_dir_all(quorum_store_db_dir).unwrap();
    } else {
        info!("don't do anything to quorum store db");
    }
    info!("start node 0");
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .start()
        .unwrap();

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();

    info!("generate traffic");
    generate_traffic_and_assert_committed(&mut swarm, &[node_to_restart]).await;

    swarm
        .wait_for_all_nodes_to_catchup(Duration::from_secs(MAX_WAIT_SECS))
        .await
        .unwrap();
}

/// Checks that a validator can still get signatures on batches on restart when the db is intact.
#[tokio::test]
async fn test_batch_id_on_restart_same_db() {
    test_batch_id_on_restart(false).await;
}

/// Checks that a validator can still get signatures on batches even if its db is reset (e.g.,
/// the disk failed, or the validator had to be moved to another node).
#[tokio::test]
async fn test_batch_id_on_restart_wiped_db() {
    test_batch_id_on_restart(true).await;
}
