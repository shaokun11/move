// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    logging::AdapterLogSchema,
    move_vm_ext::{MoveResolverExt, SessionExt, SessionId},
};
use anyhow::Result;
use aptos_aggregator::transaction::TransactionOutputExt;
use aptos_types::{
    block_metadata::BlockMetadata,
    transaction::{
        SignatureCheckedTransaction, SignedTransaction, Transaction, TransactionOutput,
        TransactionStatus, WriteSetPayload,
    },
    vm_status::{StatusCode, VMStatus},
    write_set::WriteSet,
};

/// This trait describes the VM adapter's interface.
/// TODO: bring more of the execution logic in aptos_vm into this file.
pub trait VMAdapter {
    /// Creates a new Session backed by the given storage.
    /// TODO: this doesn't belong in this trait. We should be able to remove
    /// this after redesigning cache ownership model.
    fn new_session<'r, R: MoveResolverExt>(
        &self,
        remote: &'r R,
        session_id: SessionId,
    ) -> SessionExt<'r, '_, R>;

    /// Checks the signature of the given signed transaction and returns
    /// `Ok(SignatureCheckedTransaction)` if the signature is valid.
    fn check_signature(txn: SignedTransaction) -> Result<SignatureCheckedTransaction>;

    /// Check if the transaction format is supported.
    fn check_transaction_format(&self, txn: &SignedTransaction) -> Result<(), VMStatus>;

    /// Runs the prologue for the given transaction.
    fn run_prologue<S: MoveResolverExt, SS: MoveResolverExt>(
        &self,
        session: &mut SessionExt<SS>,
        storage: &S,
        transaction: &SignatureCheckedTransaction,
        log_context: &AdapterLogSchema,
    ) -> Result<(), VMStatus>;

    /// TODO: maybe remove this after more refactoring of execution logic.
    fn should_restart_execution(output: &TransactionOutput) -> bool;

    /// Execute a single transaction.
    fn execute_single_transaction<S: MoveResolverExt>(
        &self,
        txn: &PreprocessedTransaction,
        data_cache: &S,
        log_context: &AdapterLogSchema,
    ) -> Result<(VMStatus, TransactionOutputExt, Option<String>), VMStatus>;

    fn validate_signature_checked_transaction<S: MoveResolverExt, SS: MoveResolverExt>(
        &self,
        session: &mut SessionExt<SS>,
        storage: &S,
        transaction: &SignatureCheckedTransaction,
        allow_too_new: bool,
        log_context: &AdapterLogSchema,
    ) -> Result<(), VMStatus> {
        self.check_transaction_format(transaction)?;

        let prologue_status = self.run_prologue(session, storage, transaction, log_context);
        match prologue_status {
            Err(err)
                if !allow_too_new || err.status_code() != StatusCode::SEQUENCE_NUMBER_TOO_NEW =>
            {
                Err(err)
            },
            _ => Ok(()),
        }
    }
}

/// Transactions after signature checking:
/// Waypoints and BlockPrologues are not signed and are unaffected by signature checking,
/// but a user transaction or writeset transaction is transformed to a SignatureCheckedTransaction.
#[derive(Debug)]
pub enum PreprocessedTransaction {
    UserTransaction(Box<SignatureCheckedTransaction>),
    WaypointWriteSet(WriteSetPayload),
    BlockMetadata(BlockMetadata),
    InvalidSignature,
    StateCheckpoint,
}

/// Check the signature (if any) of a transaction. If the signature is OK, the result
/// is a PreprocessedTransaction, where a user transaction is translated to a
/// SignatureCheckedTransaction and also categorized into either a UserTransaction
/// or a WriteSet transaction.
pub(crate) fn preprocess_transaction<A: VMAdapter>(txn: Transaction) -> PreprocessedTransaction {
    match txn {
        Transaction::BlockMetadata(b) => PreprocessedTransaction::BlockMetadata(b),
        Transaction::GenesisTransaction(ws) => PreprocessedTransaction::WaypointWriteSet(ws),
        Transaction::UserTransaction(txn) => {
            let checked_txn = match A::check_signature(txn) {
                Ok(checked_txn) => checked_txn,
                _ => {
                    return PreprocessedTransaction::InvalidSignature;
                },
            };
            PreprocessedTransaction::UserTransaction(Box::new(checked_txn))
        },
        Transaction::StateCheckpoint(_) => PreprocessedTransaction::StateCheckpoint,
    }
}

pub(crate) fn discard_error_vm_status(err: VMStatus) -> (VMStatus, TransactionOutputExt) {
    let vm_status = err.clone();
    let error_code = match err.keep_or_discard() {
        Ok(_) => {
            debug_assert!(false, "discarding non-discardable error: {:?}", vm_status);
            vm_status.status_code()
        },
        Err(code) => code,
    };
    (vm_status, discard_error_output(error_code))
}

pub(crate) fn discard_error_output(err: StatusCode) -> TransactionOutputExt {
    // Since this transaction will be discarded, no writeset will be included.
    TransactionOutputExt::from(TransactionOutput::new(
        WriteSet::default(),
        vec![],
        0,
        TransactionStatus::Discard(err),
    ))
}
