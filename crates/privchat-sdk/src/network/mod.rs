use crate::error::Result;
use crate::storage::queue::MessageData;
use async_trait::async_trait;
use std::fmt::Debug;

#[async_trait]
pub trait NetworkSender: Debug + Send + Sync {
    async fn send_message(&self, message: &MessageData) -> Result<()>;
}

pub struct DummyNetworkSender;

impl Debug for DummyNetworkSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DummyNetworkSender")
    }
}

#[async_trait]
impl NetworkSender for DummyNetworkSender {
    async fn send_message(&self, _message: &MessageData) -> Result<()> {
        Ok(())
    }
} 