use privchat_sdk::sdk::PrivchatSDK;
use privchat_sdk::handlers::{ConnectAckHandler, DisconnectHandler, RecvHandler, SendAckHandler};
use privchat_protocol::message::MessageType;

#[tokio::main]
async fn main() {
    let mut sdk = PrivchatSDK::new("127.0.0.1", 1234, "cert.pem".to_string());

    sdk.register_message_handler(MessageType::ConnectAck, ConnectAckHandler);
    sdk.register_message_handler(MessageType::Disconnect, DisconnectHandler);
    sdk.register_message_handler(MessageType::Recv, RecvHandler);
    sdk.register_message_handler(MessageType::SendAck, SendAckHandler);

    // 连接服务器并发送消息
    if let Err(e) = sdk.connect().await {
        eprintln!("Failed to connect: {:?}", e);
    }
}