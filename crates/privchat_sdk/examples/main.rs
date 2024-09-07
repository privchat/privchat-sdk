use msgtrans::compression::CompressionMethod;
use privchat_protocol::message::MessageType;
use privchat_protocol::message::*;
use privchat_sdk::handlers::{ConnectAckHandler, RecvHandler};
use privchat_sdk::sdk::PrivchatSDK;

#[tokio::main]
async fn main() {
    // 创建 PrivchatSDK 实例
    let mut sdk = PrivchatSDK::new("127.0.0.1", 8080, "/path/to/cert.pem".into());

    // 设置压缩类型
    sdk.set_compression_type(CompressionMethod::Zstd);

    // 注册消息处理器
    sdk.register_message_handler(MessageType::ConnectAck, ConnectAckHandler);
    sdk.register_message_handler(MessageType::Recv, RecvHandler);

    // 连接到服务器
    if let Err(err) = sdk.connect().await {
        eprintln!("Failed to connect: {}", err);
        return;
    }

    // 创建并发送消息
    let mut message = SendMessage::new();
    message.setting = Setting::new();
    message.client_seq = 12345;
    message.client_msg_no = String::from("unique_msg_no");
    message.stream_no = String::from("stream_001");
    message.channel_id = String::from("channel_01");
    message.channel_type = 1;
    message.payload = vec![1, 2, 3, 4, 5]; // 示例负荷数据

    if let Err(err) = sdk.send_message(message).await {
        eprintln!("Failed to send message: {}", err);
    } else {
        println!("Message sent successfully.");
    }

    // 保持连接
    tokio::signal::ctrl_c().await.expect("Exit for Ctrl+C");
}
