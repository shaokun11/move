// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    db_debugger::common::{parse_nibble_path, DbDir},
    jellyfish_merkle_node::JellyfishMerkleNodeSchema,
};
use anyhow::{ensure, Result};
use aptos_crypto::HashValue;
use aptos_jellyfish_merkle::node_type::{Child, Node, NodeKey, NodeType};
use aptos_types::{
    nibble::{nibble_path::NibblePath, Nibble},
    transaction::Version,
};
use clap::Parser;
use owo_colors::OwoColorize;

#[derive(Parser)]
#[clap(about = "Print nodes leading to target nibble.")]
pub struct Cmd {
    #[clap(flatten)]
    db_dir: DbDir,

    #[clap(long)]
    before_version: Version,

    #[clap(long, parse(try_from_str=parse_nibble_path))]
    nibble_path: NibblePath,
}

impl Cmd {
    pub fn run(self) -> Result<()> {
        ensure!(self.before_version > 0);
        println!(
            "{}",
            format!(
                "* Get full path from the latest root strictly before version {} to position [{:?}]. \n",
                self.before_version, self.nibble_path,
            )
            .yellow()
        );

        let db = self.db_dir.open_state_merkle_db()?;
        let mut iter = db.rev_iter::<JellyfishMerkleNodeSchema>(Default::default())?;
        iter.seek_for_prev(&NodeKey::new_empty_path(self.before_version - 1))?;
        let mut version = iter.next().transpose()?.unwrap().0.version();
        let root_version = version;

        let mut cur_pos = NibblePath::new_even(vec![]);
        let mut expected_node_hash = None;
        for nibble in self.nibble_path.nibbles() {
            match self.render_node(
                &db,
                version,
                &cur_pos,
                root_version,
                Some(nibble),
                expected_node_hash,
            )? {
                Some((ver, node_hash)) => {
                    version = ver;
                    expected_node_hash = Some(node_hash);
                },
                None => return Ok(()),
            }

            cur_pos.push(nibble);
        }
        self.render_node(
            &db,
            version,
            &cur_pos,
            root_version,
            None,
            expected_node_hash,
        )?;
        Ok(())
    }

    pub fn render_node(
        &self,
        db: &aptos_schemadb::DB,
        version: Version,
        pos: &NibblePath,
        root_version: Version,
        target_child: Option<Nibble>,
        expected_hash: Option<HashValue>,
    ) -> Result<Option<(Version, HashValue)>> {
        let node_key = NodeKey::new(version, pos.clone());
        let node = db.get::<JellyfishMerkleNodeSchema>(&node_key)?;
        let node_type = match node {
            None => "No node",
            Some(Node::Internal(_)) => "Internal node",
            Some(Node::Leaf(_)) => "Leaf node",
            Some(Node::Null) => "Null node",
        };
        println!(
            "\n {:20} created at ver: {:<20} pos: [{:?}]:",
            node_type.yellow(),
            version,
            pos
        );
        if let Some(node) = &node {
            let node_hash = node.hash();
            if let Some(expected_node_hash) = expected_hash {
                if node_hash != expected_node_hash {
                    println!(
                        "{}",
                        format!(
                            "!!! Corruption detected:\n\
                             !!!              hash: {}\n\
                             !!!     expected hash: {}
                            ",
                            node_hash, expected_node_hash,
                        )
                        .red(),
                    )
                }
            }
            println!("----------------------------------------------------------------");
        } else {
            println!("{}", "!!! Node Missing! (Could've been pruned.)".red())
        }
        let mut ret = None;
        match node {
            None => (),
            Some(Node::Internal(node)) => {
                for n in 0..16 {
                    let nibble = Nibble::from(n);
                    let is_target = Some(nibble) == target_child;
                    let child = node.child(Nibble::from(n));
                    let msg = match child {
                        None => "        ".to_string(),
                        Some(Child {
                            hash,
                            version,
                            node_type,
                        }) => {
                            let child_type = match node_type {
                                NodeType::Internal { .. } => "Internal",
                                NodeType::Leaf => "Leaf",
                                NodeType::Null => "Null",
                            };
                            if is_target {
                                ret = Some((*version, *hash));
                            }
                            format!(
                                "{:>8} {} ver:{} {}",
                                child_type,
                                hash,
                                version,
                                if root_version == *version { "*" } else { "" }.green()
                            )
                        },
                    };
                    if is_target {
                        println!(
                            "{}",
                            format!("     -> {:x} {}", nibble, msg.yellow()).yellow()
                        );
                    } else {
                        println!("        {:x} {}", nibble, msg)
                    }
                }
            },
            Some(Node::Leaf(leaf_node)) => {
                println!("           state key: {:?}\n", leaf_node.value_index().0);
                println!("    full nibble path: {:x}", leaf_node.account_key());
                println!("          value hash: {:x}", leaf_node.value_hash());
            },
            Some(Node::Null) => {
                println!("    {}", "This is a bug.".red());
            },
        }

        Ok(ret)
    }
}