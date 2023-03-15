// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    common::{Payload, PayloadFilter, Round},
    proof_of_store::LogicalTime,
};
use anyhow::Result;
use aptos_crypto::HashValue;
use futures::channel::oneshot;
use std::{fmt, fmt::Formatter};

pub enum BlockProposalCommand {
    /// Request to pull block to submit to consensus.
    GetBlockRequest(
        Round,
        // max block size
        u64,
        // max byte size
        u64,
        // block payloads to exclude from the requested block
        PayloadFilter,
        // callback to respond to
        oneshot::Sender<Result<ConsensusResponse>>,
    ),
}

impl fmt::Display for BlockProposalCommand {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            BlockProposalCommand::GetBlockRequest(round, max_txns, max_bytes, excluded, _) => {
                write!(
                    f,
                    "GetBlockRequest [round: {}, max_txns: {}, max_bytes: {} excluded: {}]",
                    round, max_txns, max_bytes, excluded
                )
            },
        }
    }
}

pub enum CleanCommand {
    CleanRequest(LogicalTime, Vec<HashValue>),
}

impl fmt::Display for CleanCommand {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            CleanCommand::CleanRequest(logical_time, digests) => {
                write!(
                    f,
                    "CleanRequest [epoch: {}, round: {}, digests: {:?}]",
                    logical_time.epoch(),
                    logical_time.round(),
                    digests
                )
            },
        }
    }
}

#[derive(Debug)]
pub enum ConsensusResponse {
    GetBlockResponse(Payload),
}
