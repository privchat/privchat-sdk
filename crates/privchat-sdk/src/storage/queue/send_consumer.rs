use crate::storage::queue::{SendQueueManager, SendTask};
use crate::network::NetworkSender;
use std::sync::Arc;
use tokio::task;
use tracing::{info, error};

pub struct SendConsumer {
    queue_manager: Arc<SendQueueManager>,
    network_sender: Arc<dyn NetworkSender>,
}

impl SendConsumer {
    pub fn new(queue_manager: Arc<SendQueueManager>, network_sender: Arc<dyn NetworkSender>) -> Self {
        Self {
            queue_manager,
            network_sender,
        }
    }

    pub async fn start(&self) {
        loop {
            if let Some(task) = self.queue_manager.dequeue_task() {
                let network_sender = self.network_sender.clone();
                task::spawn(async move {
                    match network_sender.send_message(&task.message_data).await {
                        Ok(_) => info!("Message sent successfully: {}", task.client_msg_no),
                        Err(e) => error!("Failed to send message: {}. Error: {}", task.client_msg_no, e),
                    }
                });
            }
        }
    }
} 