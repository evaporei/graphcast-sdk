use derive_getters::Getters;
use serde_derive::{Deserialize, Serialize};

use crate::graphql::client_graph_node::indexing_statuses::IndexingStatusesIndexingStatuses;
use crate::graphql::client_graph_node::{
    get_indexing_statuses, query_graph_node_network_block_hash,
};
use crate::graphql::client_network::{query_network_subgraph, Network};
use crate::graphql::client_registry::query_registry;
use crate::graphql::QueryError;

#[derive(Clone, Debug, Getters, Serialize, Deserialize, PartialEq)]
pub struct CallBook {
    /// A constant defining the graph node endpoint
    graph_node_status: String,
    /// A constant defining Graphcast registry subgraph endpoint
    graphcast_registry: String,
    /// A constant defining The Graph network subgraph endpoint
    graph_network: String,
}

impl CallBook {
    pub fn new(
        graph_node_status: String,
        graphcast_registry: String,
        graph_network: String,
    ) -> CallBook {
        CallBook {
            graph_node_status,
            graphcast_registry,
            graph_network,
        }
    }
    pub async fn block_hash(
        &self,
        network: String,
        block_number: u64,
    ) -> Result<String, QueryError> {
        query_graph_node_network_block_hash(self.graph_node_status.clone(), network, block_number)
            .await
    }

    pub async fn registered_indexer(&self, wallet_address: String) -> Result<String, QueryError> {
        query_registry(self.graphcast_registry.clone(), wallet_address).await
    }

    pub async fn indexing_statuses(
        &self,
    ) -> Result<Vec<IndexingStatusesIndexingStatuses>, QueryError> {
        get_indexing_statuses(self.graph_node_status.clone()).await
    }

    pub async fn network_subgraph(&self, indexer_address: String) -> Result<Network, QueryError> {
        query_network_subgraph(self.graph_network.clone(), indexer_address).await
    }
}
