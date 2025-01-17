use std::collections::{HashMap, HashSet};

use crate::graphql::QueryError;
use crate::NetworkPointer;
use crate::{networks::NetworkName, BlockPointer};
use graphql_client::{GraphQLQuery, Response};
use serde_derive::{Deserialize, Serialize};
use tracing::{debug, trace, warn};

use self::indexing_statuses::IndexingStatusesIndexingStatuses;

#[derive(GraphQLQuery, Serialize, Deserialize, Debug, Clone, Copy)]
#[graphql(
    schema_path = "src/graphql/schema_graph_node.graphql",
    query_path = "src/graphql/query_indexing_statuses.graphql",
    response_derives = "Debug, Serialize, Deserialize",
    normalization = "rust"
)]
pub struct IndexingStatuses;

#[derive(GraphQLQuery, Serialize, Deserialize, Debug)]
#[graphql(
    schema_path = "src/graphql/schema_graph_node.graphql",
    query_path = "src/graphql/query_block_hash_from_number.graphql",
    response_derives = "Debug, Serialize, Deserialize"
)]
pub struct BlockHashFromNumber;

/// Query graph node for Block hash
pub async fn perform_block_hash_from_number(
    graph_node_endpoint: String,
    variables: block_hash_from_number::Variables,
) -> Result<reqwest::Response, reqwest::Error> {
    let request_body = BlockHashFromNumber::build_query(variables);
    let client = reqwest::Client::new();
    client
        .post(graph_node_endpoint)
        .json(&request_body)
        .send()
        .await?
        .error_for_status()
}

/// Construct GraphQL variables and parse result for Proof of Indexing.
/// For other radio use cases, provide a function that returns a string
pub async fn query_graph_node_network_block_hash(
    graph_node_endpoint: String,
    network: String,
    block_number: u64,
) -> Result<String, QueryError> {
    let variables: block_hash_from_number::Variables = block_hash_from_number::Variables {
        network: network.clone(),
        block_number: block_number.try_into().unwrap(),
    };
    let queried_result =
        perform_block_hash_from_number(graph_node_endpoint.clone(), variables).await?;
    trace!(
        result = tracing::field::debug(&queried_result),
        "Query result for graph node network block hash"
    );
    if !queried_result.status().is_success() {
        warn!(
            result = tracing::field::debug(&queried_result),
            "Unsuccessful query"
        );
    }
    let response_body: Response<block_hash_from_number::ResponseData> =
        queried_result.json().await?;

    if let Some(data) = response_body.data {
        match data.block_hash_from_number {
            Some(hash) => Ok(hash),
            None => Err(QueryError::ParseResponseError(
                "No block hash from number".to_string(),
            )),
        }
    } else {
        Err(QueryError::ParseResponseError(format!(
            "No data for {network} blockHash at block {block_number}"
        )))
    }
}

/// Query graph node for Indexing Statuses
pub async fn perform_indexing_statuses(
    graph_node_endpoint: String,
    variables: indexing_statuses::Variables,
) -> Result<reqwest::Response, reqwest::Error> {
    let request_body = IndexingStatuses::build_query(variables);
    let client = reqwest::Client::new();
    client
        .post(graph_node_endpoint)
        .json(&request_body)
        .send()
        .await?
        .error_for_status()
}

/// This function get all indexing statuses from Graph node status endpoint
pub async fn get_indexing_statuses(
    graph_node_endpoint: String,
) -> Result<Vec<IndexingStatusesIndexingStatuses>, QueryError> {
    let variables: indexing_statuses::Variables = indexing_statuses::Variables {};
    let queried_result = perform_indexing_statuses(graph_node_endpoint.clone(), variables).await?;
    trace!(
        result = tracing::field::debug(&queried_result),
        "Query result for indexing statuses"
    );
    let response_body: Response<indexing_statuses::ResponseData> = queried_result.json().await?;

    response_body
        .data
        .map(|data| data.indexing_statuses)
        .ok_or(QueryError::IndexingError)
}

/// This function update the chainhead block pointer for each Network according to the indexingStatuses of subgraphs
pub fn update_network_chainheads(
    statuses: Vec<IndexingStatusesIndexingStatuses>,
) -> HashMap<NetworkName, BlockPointer> {
    let mut network_map: HashMap<NetworkName, BlockPointer> = HashMap::new();
    let updated_networks = statuses
        .into_iter()
        .map(|status| {
            status
                .chains
                .into_iter()
                .map(|chain| {
                    let network_name = chain.network.clone();
                    if let Some(blk) = chain.chain_head_block {
                        let blk_ptr = BlockPointer {
                            hash: blk.hash,
                            number: blk.number.as_str().parse::<u64>().unwrap_or_default(),
                        };
                        network_map
                            .entry(NetworkName::from_string(&network_name))
                            .and_modify(|block| *block = blk_ptr.clone())
                            .or_insert(blk_ptr);
                    };
                    network_name
                })
                .collect::<String>()
        })
        .collect::<HashSet<String>>();
    trace!(
        network = tracing::field::debug(&updated_networks),
        "Updated chainhead"
    );
    network_map
}

/// This function gathers the subgraph's network name and latest blocks from the indexing statuses
pub fn subgraph_network_blocks(
    statuses: Vec<IndexingStatusesIndexingStatuses>,
) -> HashMap<String, NetworkPointer> {
    // subgraph (network, latest blocks)
    let mut subgraph_network_blocks: HashMap<String, NetworkPointer> = HashMap::new();

    let updated_subgraphs = statuses
        .into_iter()
        .map(|status| {
            status
                .chains
                .into_iter()
                .map(|chain| {
                    if let Some(blk) = chain.latest_block {
                        let blk_ptr = BlockPointer {
                            hash: blk.hash,
                            number: blk.number.as_str().parse::<u64>().unwrap_or_default(),
                        };
                        subgraph_network_blocks
                            .entry(status.subgraph.clone())
                            .or_insert(NetworkPointer {
                                network: chain.network.clone(),
                                block: blk_ptr,
                            });
                    };
                    status.subgraph.clone()
                })
                .collect::<String>()
        })
        .collect::<Vec<String>>();
    debug!(
        number_of_subgraphs = updated_subgraphs.len(),
        "Updated latest block pointers for subgraphs",
    );
    subgraph_network_blocks
}
