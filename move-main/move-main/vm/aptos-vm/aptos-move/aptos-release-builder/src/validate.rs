// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::ReleaseConfig;
use anyhow::Result;
use aptos_api_types::U64;
use aptos_crypto::ed25519::Ed25519PrivateKey;
use aptos_genesis::keys::PrivateIdentity;
use aptos_rest_client::Client;
use aptos_temppath::TempPath;
use aptos_types::account_address::AccountAddress;
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    thread::sleep,
    time::Duration,
};
use url::Url;

#[derive(Clone, Debug)]
pub struct NetworkConfig {
    pub endpoint: Url,
    pub root_key_path: PathBuf,
    pub validator_account: AccountAddress,
    pub validator_key: Ed25519PrivateKey,
}

#[derive(Deserialize)]
struct CreateProposalEvent {
    proposal_id: U64,
}

const FAST_VOTE_SCRIPT: &str = r"
script {
    use aptos_framework::aptos_governance;

    fun main(core_resources: &signer) {
        let core_signer = aptos_governance::get_signer_testnet_only(core_resources, @0000000000000000000000000000000000000000000000000000000000000001);

        let framework_signer = &core_signer;

        aptos_governance::update_governance_config(framework_signer, 0, 0, 30);
    }
}
";

impl NetworkConfig {
    pub fn new_from_dir(endpoint: Url, test_dir: &Path) -> Result<Self> {
        let root_key_path = test_dir.join("mint.key");
        let private_identity_file = test_dir.join("0/private-identity.yaml");
        let private_identity =
            serde_yaml::from_slice::<PrivateIdentity>(&fs::read(private_identity_file)?)?;

        Ok(Self {
            endpoint,
            root_key_path,
            validator_account: private_identity.account_address,
            validator_key: private_identity.account_private_key,
        })
    }

    /// Submit all govenerance proposal script inside script_path to the corresponding rest endpoint.
    ///
    /// For all script, we will:
    /// - Generate a governance proposal and get its proposal id
    /// - Use validator's privkey to vote for this proposal
    /// - Add the proposal to allow list using validator account
    /// - Execute this proposal
    ///
    /// We expect all the scripts here to be single step governance proposal.
    pub async fn submit_and_execute_proposal(&self, script_path: Vec<PathBuf>) -> Result<()> {
        let mut proposals = vec![];
        self.mint_to_validator()?;
        for path in script_path.iter() {
            let proposal_id = self
                .create_governance_proposal(path.as_path(), false)
                .await?;
            self.vote_proposal(proposal_id)?;
            proposals.push(proposal_id);
        }

        // Wait for the voting period to pass
        sleep(Duration::from_secs(40));
        for (proposal_id, path) in proposals.iter().zip(script_path.iter()) {
            self.add_proposal_to_allow_list(*proposal_id)?;
            self.execute_proposal(*proposal_id, path.as_path())?;
        }
        Ok(())
    }

    /// Submit all govenerance proposal script inside script_path to the corresponding rest endpoint.
    ///
    /// - We will first submit a governance proposal for the first script (in alphabetical order).
    /// - Validator will vote for this proposal
    ///
    /// Once voting period has passed, we should be able to execute all the scripts in the folder in alphabetical order.
    /// We expect all the scripts here to be multi step governance proposal.
    pub async fn submit_and_execute_multi_step_proposal(
        &self,
        script_path: Vec<PathBuf>,
    ) -> Result<()> {
        let first_script = script_path.first().unwrap();
        self.mint_to_validator()?;
        let proposal_id = self
            .create_governance_proposal(first_script.as_path(), true)
            .await?;
        self.vote_proposal(proposal_id)?;
        // Wait for the proposal to resolve.
        sleep(Duration::from_secs(40));
        for path in script_path {
            self.add_proposal_to_allow_list(proposal_id)?;
            self.execute_proposal(proposal_id, path.as_path())?;
        }
        Ok(())
    }

    /// Change the network to resolve governance proposal within 30 secs.
    pub fn set_fast_resolve(&self) -> Result<()> {
        let fast_resolve_script = aptos_temppath::TempPath::new();
        fast_resolve_script.create_as_file()?;
        let mut fas_script_path = fast_resolve_script.path().to_path_buf();
        fas_script_path.set_extension("move");

        std::fs::write(fas_script_path.as_path(), FAST_VOTE_SCRIPT.as_bytes())?;

        let mut root_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        root_path.pop();
        root_path.pop();

        let framework_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("aptos-move")
            .join("framework")
            .join("aptos-framework");

        let args = vec![
            "run",
            "--bin",
            "aptos",
            "--",
            "move",
            "run-script",
            "--script-path",
            fas_script_path.as_path().to_str().unwrap(),
            "--sender-account",
            "0xa550c18",
            "--private-key-file",
            self.root_key_path.as_path().to_str().unwrap(),
            "--framework-local-dir",
            framework_path.as_path().to_str().unwrap(),
            "--assume-yes",
            "--encoding",
            "bcs",
            "--url",
            self.endpoint.as_str(),
        ];
        assert!(std::process::Command::new("cargo")
            .current_dir(root_path.as_path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .args(args)
            .output()?
            .status
            .success());
        Ok(())
    }

    pub async fn create_governance_proposal(
        &self,
        script_path: &Path,
        is_multi_step: bool,
    ) -> Result<u64> {
        let address_string = format!("{}", self.validator_account);
        let privkey_string = hex::encode(self.validator_key.to_bytes());
        let mut root_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        root_path.pop();
        root_path.pop();

        let framework_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("aptos-move")
            .join("framework")
            .join("aptos-framework");

        let mut args = vec![
            "run",
            "--bin",
            "aptos",
            "--",
            "governance",
            "propose",
            "--pool-address",
            address_string.as_str(),
            "--script-path",
            script_path.to_str().unwrap(),
            "--framework-local-dir",
            framework_path.as_path().to_str().unwrap(),
            "--metadata-url",
            "https://raw.githubusercontent.com/aptos-labs/aptos-core/b4fb9acfc297327c43d030def2b59037c4376611/testsuite/smoke-test/src/upgrade_multi_step_test_metadata.txt",
            "--sender-account",
            address_string.as_str(),
            "--private-key",
            privkey_string.as_str(),
            "--url",
            self.endpoint.as_str(),
            "--assume-yes",
        ];

        if is_multi_step {
            args.push("--is-multi-step");
        }

        assert!(std::process::Command::new("cargo")
            .current_dir(root_path.as_path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .args(args)
            .output()?
            .status
            .success());

        // Get proposal id.
        let event = Client::new(self.endpoint.clone())
            .get_account_events(
                AccountAddress::ONE,
                "0x1::aptos_governance::GovernanceEvents",
                "create_proposal_events",
                None,
                Some(100),
            )
            .await?
            .into_inner()
            .pop()
            .unwrap();

        Ok(*serde_json::from_value::<CreateProposalEvent>(event.data)?
            .proposal_id
            .inner())
    }

    pub fn vote_proposal(&self, proposal_id: u64) -> Result<()> {
        let address_string = format!("{}", self.validator_account);
        let privkey_string = hex::encode(self.validator_key.to_bytes());
        let proposal_id = format!("{}", proposal_id);
        let mut root_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        root_path.pop();
        root_path.pop();

        let args = vec![
            "run",
            "--bin",
            "aptos",
            "--",
            "governance",
            "vote",
            "--pool-addresses",
            address_string.as_str(),
            "--sender-account",
            address_string.as_str(),
            "--private-key",
            privkey_string.as_str(),
            "--assume-yes",
            "--proposal-id",
            proposal_id.as_str(),
            "--yes",
            "--url",
            self.endpoint.as_str(),
        ];
        assert!(std::process::Command::new("cargo")
            .current_dir(root_path.as_path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .args(args)
            .output()?
            .status
            .success());
        Ok(())
    }

    pub fn mint_to_validator(&self) -> Result<()> {
        let address_args = format!("address:{}", self.validator_account);
        let mut root_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        root_path.pop();
        root_path.pop();

        println!("Minting to validator account");
        let args = vec![
            "run",
            "--bin",
            "aptos",
            "--",
            "move",
            "run",
            "--function-id",
            "0x1::aptos_coin::mint",
            "--sender-account",
            "0xa550c18",
            "--args",
            address_args.as_str(),
            "u64:100000000000",
            "--private-key-file",
            self.root_key_path.as_path().to_str().unwrap(),
            "--assume-yes",
            "--encoding",
            "bcs",
            "--url",
            self.endpoint.as_str(),
        ];
        assert!(std::process::Command::new("cargo")
            .current_dir(root_path.as_path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .args(args)
            .output()?
            .status
            .success());
        Ok(())
    }

    pub fn add_proposal_to_allow_list(&self, proposal_id: u64) -> Result<()> {
        let proposal_id = format!("u64:{}", proposal_id);
        let mut root_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        root_path.pop();
        root_path.pop();

        let args = vec![
            "run",
            "--bin",
            "aptos",
            "--",
            "move",
            "run",
            "--function-id",
            "0x1::aptos_governance::add_approved_script_hash_script",
            "--sender-account",
            "0xa550c18",
            "--args",
            proposal_id.as_str(),
            "--private-key-file",
            self.root_key_path.as_path().to_str().unwrap(),
            "--assume-yes",
            "--encoding",
            "bcs",
            "--url",
            self.endpoint.as_str(),
        ];
        assert!(std::process::Command::new("cargo")
            .current_dir(root_path.as_path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .args(args)
            .output()?
            .status
            .success());
        Ok(())
    }

    pub fn execute_proposal(&self, proposal_id: u64, script_path: &Path) -> Result<()> {
        let address_string = format!("{}", self.validator_account);
        let privkey_string = hex::encode(self.validator_key.to_bytes());
        let proposal_id = format!("{}", proposal_id);
        let mut root_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        root_path.pop();
        root_path.pop();

        let framework_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("aptos-move")
            .join("framework")
            .join("aptos-framework");

        let args = vec![
            "run",
            "--bin",
            "aptos",
            "--",
            "governance",
            "execute-proposal",
            "--proposal-id",
            proposal_id.as_str(),
            "--script-path",
            script_path.to_str().unwrap(),
            "--framework-local-dir",
            framework_path.as_path().to_str().unwrap(),
            "--sender-account",
            address_string.as_str(),
            "--private-key",
            privkey_string.as_str(),
            "--assume-yes",
            "--url",
            self.endpoint.as_str(),
            // Use the max gas unit for now. The simulate API sometimes cannot get the right gas estimate for proposals.
            "--max-gas",
            "2000000",
        ];
        assert!(std::process::Command::new("cargo")
            .current_dir(root_path.as_path())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .args(args)
            .output()?
            .status
            .success());
        Ok(())
    }
}

async fn execute_release(
    release_config: ReleaseConfig,
    network_config: NetworkConfig,
) -> Result<()> {
    let scripts_path = TempPath::new();
    scripts_path.create_as_dir()?;

    let mut root_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    root_path.pop();
    root_path.pop();

    let framework_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("aptos-move")
        .join("framework")
        .join("aptos-framework");

    release_config.generate_release_proposal_scripts(scripts_path.path())?;

    let mut script_paths: Vec<PathBuf> = std::fs::read_dir(scripts_path.path())?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().map(|s| s == "move").unwrap_or(false) {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    script_paths.sort();

    if release_config.is_multi_step {
        network_config.set_fast_resolve()?;
        network_config
            .submit_and_execute_multi_step_proposal(script_paths)
            .await?;
    } else if release_config.testnet {
        for entry in script_paths {
            println!("Executing: {:?}", entry);
            let args = vec![
                "run",
                "--bin",
                "aptos",
                "--",
                "move",
                "run-script",
                "--script-path",
                entry.as_path().to_str().unwrap(),
                "--sender-account",
                "0xa550c18",
                "--private-key-file",
                network_config.root_key_path.as_path().to_str().unwrap(),
                "--framework-local-dir",
                framework_path.as_path().to_str().unwrap(),
                "--assume-yes",
                "--encoding",
                "bcs",
                "--url",
                network_config.endpoint.as_str(),
            ];
            assert!(std::process::Command::new("cargo")
                .current_dir(root_path.as_path())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .args(args)
                .output()?
                .status
                .success());
        }
    } else {
        network_config.set_fast_resolve()?;
        // Single step governance proposal;
        network_config
            .submit_and_execute_proposal(script_paths)
            .await?;
    };
    Ok(())
}

pub async fn validate_config(
    release_config: ReleaseConfig,
    network_config: NetworkConfig,
) -> Result<()> {
    execute_release(release_config.clone(), network_config.clone()).await?;
    release_config.validate_upgrade(network_config.endpoint)
}
