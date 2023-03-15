// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

extern crate core;

mod backup;
mod debugger;
mod replay_verify;
mod restore;
#[cfg(test)]
mod tests;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[clap(name = "Aptos db tool", author, version, propagate_version = true)]
pub enum DBTool {
    #[clap(subcommand)]
    Backup(backup::Command),
    #[clap(subcommand)]
    Restore(restore::Command),
    ReplayVerify(replay_verify::Opt),
    #[clap(subcommand)]
    Debug(debugger::Command),
}

impl DBTool {
    pub async fn run(self) -> Result<()> {
        match self {
            DBTool::Backup(cmd) => cmd.run().await,
            DBTool::Restore(cmd) => cmd.run().await,
            DBTool::ReplayVerify(cmd) => cmd.run().await,
            DBTool::Debug(cmd) => cmd.run(),
        }
    }
}
