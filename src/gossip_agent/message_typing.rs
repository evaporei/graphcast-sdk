use anyhow::anyhow;
use chrono::Utc;
use colored::Colorize;
use ethers::signers::{Signer, Wallet};
use ethers_core::{k256::ecdsa::SigningKey, types::Signature};
use num_bigint::BigUint;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    error::Error,
    str::FromStr,
    sync::{Arc, Mutex},
};
use waku::{Running, WakuContentTopic, WakuMessage, WakuNodeHandle, WakuPubSubTopic};

use crate::{
    graphql::{client_network::query_network_subgraph, client_registry::query_registry_indexer},
    NoncesMap,
};

use super::MSG_REPLAY_LIMIT;

/// Prepare sender:nonce to update
fn prepare_nonces(
    nonces_per_subgraph: &HashMap<String, i64>,
    address: String,
    nonce: i64,
) -> HashMap<std::string::String, i64> {
    let mut updated_nonces = HashMap::new();
    updated_nonces.clone_from(nonces_per_subgraph);
    updated_nonces.insert(address, nonce);
    updated_nonces
}

pub async fn get_indexer_stake(
    address: String,
    network_subgraph: &str,
) -> Result<BigUint, anyhow::Error> {
    Ok(
        query_network_subgraph(network_subgraph.to_string(), address.clone())
            .await?
            .indexer_stake(),
    )
}

/// GraphcastMessage type casts over radio payload
#[derive(Clone, Message, Serialize, Deserialize)]
pub struct GraphcastMessage<T>
where
    T: Message + ethers::types::transaction::eip712::Eip712 + Default + Clone + 'static,
{
    /// Graph identifier for the entity the radio is communicating about
    #[prost(string, tag = "1")]
    pub identifier: String,
    /// content to share about the identified entity
    #[prost(message, tag = "2")]
    pub payload: Option<T>,
    /// nonce cached to check against the next incoming message
    #[prost(int64, tag = "3")]
    pub nonce: i64,
    /// block relevant to the message
    #[prost(uint64, tag = "4")]
    pub block_number: u64,
    /// block hash generated from the block number
    #[prost(string, tag = "5")]
    pub block_hash: String,
    /// signature over radio payload
    #[prost(string, tag = "6")]
    pub signature: String,
}

impl<T: Message + ethers::types::transaction::eip712::Eip712 + Default + Clone + 'static>
    GraphcastMessage<T>
{
    /// Create a graphcast message
    pub fn new(
        identifier: String,
        payload: Option<T>,
        nonce: i64,
        block_number: i64,
        block_hash: String,
        signature: String,
    ) -> Self {
        GraphcastMessage {
            identifier,
            payload,
            nonce,
            block_number: block_number.try_into().unwrap(),
            block_hash,
            signature,
        }
    }

    /// Signs the radio payload and construct graphcast message
    pub async fn build(
        wallet: &Wallet<SigningKey>,
        identifier: String,
        payload: Option<T>,
        block_number: i64,
        block_hash: String,
    ) -> Result<Self, Box<dyn Error>> {
        println!("\n{}", "Constructing message".green());
        let sig = wallet
            .sign_typed_data(
                payload
                    .as_ref()
                    .expect("Radio payload failed to satisfy the defined Eip712 typing"),
            )
            .await?;

        let message = GraphcastMessage::new(
            identifier.clone(),
            payload,
            Utc::now().timestamp(),
            block_number,
            block_hash.to_string(),
            sig.to_string(),
        );

        println!("{}{:#?}", "Encode message: ".cyan(), message,);
        Ok(message)
    }

    /// Send Graphcast message to the Waku relay network
    pub fn send_to_waku(
        &self,
        node_handle: &WakuNodeHandle<Running>,
        pub_sub_topic: Option<WakuPubSubTopic>,
        content_topic: &WakuContentTopic,
    ) -> Result<String, Box<dyn Error>> {
        let mut buff = Vec::new();
        Message::encode(self, &mut buff).expect("Could not encode :(");

        let waku_message = WakuMessage::new(
            buff,
            content_topic.clone(),
            2,
            Utc::now().timestamp() as usize,
        );

        Ok(node_handle.relay_publish_message(&waku_message, pub_sub_topic, None)?)
    }

    /// Check message from valid sender: resolve indexer address and self stake
    pub async fn valid_sender(
        &self,
        registry_subgraph: &str,
        network_subgraph: &str,
    ) -> Result<&Self, anyhow::Error> {
        let indexer_address = query_registry_indexer(
            registry_subgraph.to_string(),
            self.recover_sender_address()?,
        )
        .await?;
        if query_network_subgraph(network_subgraph.to_string(), indexer_address.clone())
            .await?
            .stake_satisfy_requirement()
        {
            println!("Valid Indexer:  {}", indexer_address);
            Ok(self)
        } else {
            Err(anyhow!(
                "Sender stake is less than the minimum requirement, drop message"
            ))
        }
    }

    /// Check timestamp: prevent past message replay
    pub fn valid_time(&self) -> Result<&Self, anyhow::Error> {
        //Can store for measuring overall gossip message latency
        let message_age = Utc::now().timestamp() - self.nonce;
        // 0 allow instant atomic messaging, use 1 to exclude them
        if (0..MSG_REPLAY_LIMIT).contains(&message_age) {
            Ok(self)
        } else {
            Err(anyhow!(
                "Message timestamp {} outside acceptable range {}, drop message",
                message_age,
                MSG_REPLAY_LIMIT
            ))
        }
    }

    /// Check timestamp: prevent messages with incorrect provider
    pub fn valid_hash(&self, block_hash: String) -> Result<&Self, anyhow::Error> {
        if self.block_hash == block_hash {
            Ok(self)
        } else {
            Err(anyhow!(
                "Message hash ({}) differ from trusted provider response ({}), drop message",
                self.block_hash,
                block_hash
            ))
        }
    }

    //TODO: update to Result<>
    /// Recover sender address from Graphcast message radio payload
    pub fn recover_sender_address(&self) -> Result<String, anyhow::Error> {
        match Signature::from_str(&self.signature).and_then(|sig| {
            sig.recover(
                self.payload
                    .as_ref()
                    .expect("No payload in the radio message, just a ping")
                    .encode_eip712()
                    .unwrap(),
            )
        }) {
            Ok(addr) => Ok(format!("{:#x}", addr)),
            Err(x) => Err(anyhow!(x)),
        }
    }

    /// Check historic nonce: ensure message sequencing
    pub fn valid_nonce(&self, nonces: &Arc<Mutex<NoncesMap>>) -> Result<&Self, anyhow::Error> {
        let address = self.recover_sender_address()?;

        let mut nonces = nonces.lock().unwrap();
        let nonces_per_subgraph = nonces.get(self.identifier.clone().as_str());

        match nonces_per_subgraph {
            Some(nonces_per_subgraph) => {
                let nonce = nonces_per_subgraph.get(&address);
                match nonce {
                    Some(nonce) => {
                        println!(
                            "Latest saved nonce for subgraph {} and address {}: {}",
                            self.identifier, address, nonce
                        );

                        if nonce > &self.nonce {
                            Err(anyhow!(
                                    "Invalid nonce for subgraph {} and address {}! Received nonce - {} is smaller than currently saved one - {}, skipping message...",
                                    self.identifier, address, self.nonce, nonce
                                ))
                        } else {
                            let updated_nonces =
                                prepare_nonces(nonces_per_subgraph, address, self.nonce);
                            nonces.insert(self.identifier.clone(), updated_nonces);
                            Ok(self)
                        }
                    }
                    None => {
                        let updated_nonces =
                            prepare_nonces(nonces_per_subgraph, address.clone(), self.nonce);
                        nonces.insert(self.identifier.clone(), updated_nonces);
                        Err(anyhow!(
                                    "No saved nonce for address {} on topic {}, saving this one and skipping message...",
                                    address, self.identifier
                                ))
                    }
                }
            }
            None => {
                let updated_nonces = prepare_nonces(&HashMap::new(), address, self.nonce);
                nonces.insert(self.identifier.clone(), updated_nonces);
                Err(anyhow!(
                            "First time receiving message for subgraph {}. Saving sender and nonce, skipping message...",
                            self.identifier
                        ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers_contract::EthAbiType;
    use ethers_core::rand::thread_rng;
    use ethers_core::types::transaction::eip712::Eip712;
    use ethers_derive_eip712::*;
    use prost::Message;
    use serde::{Deserialize, Serialize};

    /// Make a test radio type
    #[derive(Eip712, EthAbiType, Clone, Message, Serialize, Deserialize)]
    #[eip712(
        name = "Graphcast Test Radio",
        version = "0",
        chain_id = 1,
        verifying_contract = "0xc944e90c64b2c07662a292be6244bdf05cda44a7"
    )]
    pub struct RadioPayloadMessage {
        #[prost(string, tag = "1")]
        pub identifier: String,
        #[prost(string, tag = "2")]
        pub content: String,
    }

    impl RadioPayloadMessage {
        pub fn new(identifier: String, content: String) -> Self {
            RadioPayloadMessage {
                identifier,
                content,
            }
        }

        pub fn content_string(&self) -> String {
            self.content.clone()
        }
    }

    /// Create a random wallet
    fn dummy_wallet() -> Wallet<SigningKey> {
        Wallet::new(&mut thread_rng())
    }

    #[tokio::test]
    async fn test_standard_message() {
        let registry_subgraph =
            "https://api.thegraph.com/subgraphs/name/hopeyen/gossip-registry-test";
        let network_subgraph = "https://gateway.testnet.thegraph.com/network";

        let hash: String = "Qmtest".to_string();
        let content: String = "0x0000".to_string();
        let payload: RadioPayloadMessage = RadioPayloadMessage::new(hash.clone(), content.clone());
        let block_number: i64 = 0;
        let block_hash: String = "0xblahh".to_string();

        let wallet = dummy_wallet();
        let msg = GraphcastMessage::build(&wallet, hash, Some(payload), block_number, block_hash)
            .await
            .unwrap();

        assert_eq!(msg.block_number, 0);
        assert!(msg
            .valid_sender(registry_subgraph, network_subgraph)
            .await
            .is_err());
        assert!(msg.valid_time().is_ok());
        assert!(msg.valid_hash("weeelp".to_string()).is_err());
        assert_eq!(msg.payload.as_ref().unwrap().content_string(), content);
        assert_eq!(
            msg.recover_sender_address().unwrap(),
            format!("{:#x}", wallet.address())
        );
    }
}
