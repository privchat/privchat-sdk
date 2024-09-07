use super::handlers::MessageHandler;
use super::message::Message;
use privchat_protocol::Protocol;
use privchat_protocol::message::*;
use msgtrans::client::MessageTransportClient;
use msgtrans::channel::QuicClientChannel;
use msgtrans::packet::{Packet, PacketHeader};
use msgtrans::compression::CompressionMethod;
use std::collections::HashMap;

type MessageHandlerMap = HashMap<MessageType, Box<dyn MessageHandler>>;

#[derive(Clone)]
pub struct PrivchatSDK {
    client: MessageTransportClient<QuicClientChannel>,
    protocol: Protocol,
    handlers: MessageHandlerMap,
    compression_type: CompressionMethod,
}

impl PrivchatSDK {
    pub fn new(address: &str, port: u16, cert_path: String) -> Self {
        let mut client = MessageTransportClient::new();
        let protocol = Protocol::new();

        let quic_channel = QuicClientChannel::new(address, port, cert_path);
        client.set_channel(quic_channel);

        let handlers = HashMap::new();

        let sdk = PrivchatSDK { 
            client: client.clone(), 
            protocol,
            handlers,
            compression_type: CompressionMethod::None
        };

        let sdk_clone = sdk.clone();
        client.set_message_handler(move |packet| {
            sdk_clone.handle_message(packet);
        });

        sdk
    }

    pub async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.client.connect().await
    }

    fn handle_message(&self, packet: Packet) {
        println!(
            "Received packet with ID: {}, Payload: {:?}",
            packet.header.message_id,
            packet.payload
        );

        match MessageType::try_from(packet.header.message_id) {
            Ok(msg_type) => {
                if let Some(handler) = self.handlers.get(&msg_type) {
                    if let Some(message) = self.protocol.decode::<Box<dyn Message>>(&packet.payload) {
                        handler.handle_message(message);
                    } else {
                        println!("Failed to decode message of type {:?}", msg_type);
                    }
                } else {
                    println!("No handler registered for message type {:?}", msg_type);
                }
            },
            Err(_) => println!("Invalid message ID"),
        }
    }

    pub fn register_message_handler<T: MessageHandler + 'static>(&mut self, message_type: MessageType, handler: T) {
        self.handlers.insert(message_type, Box::new(handler));
    }

    pub fn set_compression_type(&mut self, compression_type: CompressionMethod) {
        self.compression_type = compression_type;
    }

    pub async fn send_message(&self, message: SendMessage) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let encoded_send_message = self.protocol.encode::<SendMessage>(&message);

        let packet_header = PacketHeader {
            message_id: MessageType::Send.to_u32(),
            message_length: encoded_send_message.len() as u32,
            compression_type: self.compression_type,
            extend_length: 0,
        };

        let packet = Packet::new(packet_header, Vec::new(), encoded_send_message);
        self.client.send(packet).await
    }
}