// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

pub mod checkpoint;
mod common;
pub mod ledger;
pub mod state_tree;
pub mod truncate;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
pub enum Cmd {
    #[clap(subcommand)]
    StateTree(state_tree::Cmd),

    Checkpoint(checkpoint::Cmd),

    #[clap(subcommand)]
    Ledger(ledger::Cmd),

    Truncate(truncate::Cmd),
}

impl Cmd {
    pub fn run(self) -> Result<()> {
        match self {
            Cmd::StateTree(cmd) => cmd.run(),
            Cmd::Checkpoint(cmd) => cmd.run(),
            Cmd::Ledger(cmd) => cmd.run(),
            Cmd::Truncate(cmd) => cmd.run(),
        }
    }
}
