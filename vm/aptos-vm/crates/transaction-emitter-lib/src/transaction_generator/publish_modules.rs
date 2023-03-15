// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0
use crate::transaction_generator::{
    publishing::publish_util::PackageHandler, TransactionGenerator, TransactionGeneratorCreator,
};
use aptos_infallible::RwLock;
use aptos_sdk::{
    transaction_builder::TransactionFactory,
    types::{transaction::SignedTransaction, LocalAccount},
};
use async_trait::async_trait;
use rand::{rngs::StdRng, SeedableRng};
use std::sync::Arc;

#[allow(dead_code)]
pub struct PublishPackageGenerator {
    rng: StdRng,
    package_handler: Arc<RwLock<PackageHandler>>,
    txn_factory: TransactionFactory,
}

impl PublishPackageGenerator {
    pub fn new(
        rng: StdRng,
        package_handler: Arc<RwLock<PackageHandler>>,
        txn_factory: TransactionFactory,
    ) -> Self {
        Self {
            rng,
            package_handler,
            txn_factory,
        }
    }
}

#[async_trait]
impl TransactionGenerator for PublishPackageGenerator {
    fn generate_transactions(
        &mut self,
        accounts: Vec<&mut LocalAccount>,
        transactions_per_account: usize,
    ) -> Vec<SignedTransaction> {
        let mut requests = Vec::with_capacity(accounts.len() * transactions_per_account);
        for account in accounts {
            // First publish the module and then use it
            let package = self
                .package_handler
                .write()
                .pick_package(&mut self.rng, account);
            let txn = package.publish_transaction(account, &self.txn_factory);
            requests.push(txn);
            // use module published
            // for _ in 1..transactions_per_account - 1 {
            for _ in 1..transactions_per_account {
                let request =
                    package.use_random_transaction(&mut self.rng, account, &self.txn_factory);
                requests.push(request);
            }
            // republish
            // let package = self
            //     .package_handler
            //     .write()
            //     .pick_package(&mut self.rng, account);
            // let txn = package.publish_transaction(account, &self.txn_factory);
            // requests.push(txn);
        }
        requests
    }
}

pub struct PublishPackageCreator {
    txn_factory: TransactionFactory,
    package_handler: Arc<RwLock<PackageHandler>>,
}

impl PublishPackageCreator {
    pub fn new(txn_factory: TransactionFactory) -> Self {
        Self {
            txn_factory,
            package_handler: Arc::new(RwLock::new(PackageHandler::new())),
        }
    }
}

#[async_trait]
impl TransactionGeneratorCreator for PublishPackageCreator {
    async fn create_transaction_generator(&mut self) -> Box<dyn TransactionGenerator> {
        Box::new(PublishPackageGenerator::new(
            StdRng::from_entropy(),
            self.package_handler.clone(),
            self.txn_factory.clone(),
        ))
    }
}
