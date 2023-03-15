// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::{
    protocols::wire::handshake::v1::{ProtocolId, ProtocolIdSet},
    transport::ConnectionMetadata,
};
use serde::{Deserialize, Serialize};

/// The current connection state of a peer
/// TODO: Allow nodes that are unhealthy to stay connected
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ConnectionState {
    Connected,
    Disconnecting,
    Disconnected, // Currently unused (TODO: fix this!)
}

/// A container holding all relevant metadata for the peer.
// TODO: add more metadata once we've integrated the monitoring service
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerMetadata {
    pub(crate) connection_state: ConnectionState,
    pub(crate) connection_metadata: ConnectionMetadata,
}

impl PeerMetadata {
    pub fn new(connection_metadata: ConnectionMetadata) -> Self {
        PeerMetadata {
            connection_state: ConnectionState::Connected,
            connection_metadata,
        }
    }

    /// Returns true iff the peer is still connected
    pub fn is_connected(&self) -> bool {
        self.connection_state == ConnectionState::Connected
    }

    /// Returns true iff the peer has advertised support for the given protocol
    pub fn supports_protocol(&self, protocol_id: ProtocolId) -> bool {
        self.connection_metadata
            .application_protocols
            .contains(protocol_id)
    }

    /// Returns true iff the peer has advertised support for at least
    /// one of the given protocols.
    pub fn supports_any_protocol(&self, protocol_ids: &[ProtocolId]) -> bool {
        let protocol_id_set = ProtocolIdSet::from_iter(protocol_ids);
        !self
            .connection_metadata
            .application_protocols
            .intersect(&protocol_id_set)
            .is_empty()
    }

    /// Returns the set of supported protocols for the peer
    pub fn get_supported_protocols(&self) -> ProtocolIdSet {
        self.connection_metadata.application_protocols.clone()
    }

    /// Returns a copy of the connection metadata
    pub fn get_connection_medata(&self) -> ConnectionMetadata {
        self.connection_metadata.clone()
    }
}
