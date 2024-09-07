use privchat_protocol::message::*;


use privchat_protocol::Protocol;
use msgtrans::client::MessageTransportClient;
use msgtrans::channel::QuicClientChannel;
use msgtrans::packet::{Packet, PacketHeader};
use msgtrans::compression::CompressionMethod;

/// 定义一个消息的 trait
pub trait Message {
    fn get_message_type(&self) -> MessageType;
    fn as_any(&self) -> &dyn std::any::Any;
}

/// 定义一个消息处理器的 trait
trait MessageHandler: Clone { // 添加 Clone 约束
    fn handle_message(&self, message: Box<dyn Message>);
}

/// 创建一个新的 trait，继承自 MessageHandler 和 Clone
trait ClonableMessageHandler: MessageHandler {}

/// 实现具体的处理器
struct ConnectAckHandler;
impl MessageHandler for ConnectAckHandler { // ConnectAckHandler 需要实现 Clone
    fn handle_message(&self, message: Box<dyn Message>) {
        if let Some(connect_ack) = message.as_any().downcast_ref::<ConnectAckMessage>() {
            println!("Handling ConnectAckMessage: {:?}", connect_ack);
        }
    }
}

impl Clone for ConnectAckHandler { // 实现 Clone
    fn clone(&self) -> Self {
        Self
    }
}

impl ClonableMessageHandler for ConnectAckHandler {}

struct DisconnectHandler;
impl MessageHandler for DisconnectHandler { // DisconnectHandler 需要实现 Clone
    fn handle_message(&self, message: Box<dyn Message>) {
        if let Some(disconnect) = message.as_any().downcast_ref::<DisconnectMessage>() {
            println!("Handling DisconnectMessage: {:?}", disconnect);
        }
    }
}

impl Clone for DisconnectHandler { // 实现 Clone
    fn clone(&self) -> Self {
        Self
    }
}

impl ClonableMessageHandler for DisconnectHandler {}

struct RecvHandler;
impl MessageHandler for RecvHandler { // RecvHandler 需要实现 Clone
    fn handle_message(&self, message: Box<dyn Message>) {
        if let Some(recv) = message.as_any().downcast_ref::<RecvMessage>() {
            println!("Handling RecvMessage: {:?}", recv);
        }
    }
}

impl Clone for RecvHandler { // 实现 Clone
    fn clone(&self) -> Self {
        Self
    }
}

impl ClonableMessageHandler for RecvHandler {}

struct SendAckHandler;
impl MessageHandler for SendAckHandler { // SendAckHandler 需要实现 Clone
    fn handle_message(&self, message: Box<dyn Message>) {
        if let Some(send_ack) = message.as_any().downcast_ref::<SendAckMessage>() {
            println!("Handling SendAckMessage: {:?}", send_ack);
        }
    }
}

impl Clone for SendAckHandler { // 实现 Clone
    fn clone(&self) -> Self {
        Self
    }
}

impl ClonableMessageHandler for SendAckHandler {}

/// 定义一个枚举来表示所有可能的处理器类型
enum HandlerEnum {
    ConnectAck(ConnectAckHandler),
    Disconnect(DisconnectHandler),
    Recv(RecvHandler),
    SendAck(SendAckHandler),
}

impl MessageHandler for HandlerEnum {
    fn handle_message(&self, message: Box<dyn Message>) {
        match self {
            HandlerEnum::ConnectAck(h) => h.handle_message(message),
            HandlerEnum::Disconnect(h) => h.handle_message(message),
            HandlerEnum::Recv(h) => h.handle_message(message),
            HandlerEnum::SendAck(h) => h.handle_message(message),
        }
    }
}

impl Clone for HandlerEnum {
    fn clone(&self) -> Self {
        match self {
            HandlerEnum::ConnectAck(h) => HandlerEnum::ConnectAck(h.clone()),
            HandlerEnum::Disconnect(h) => HandlerEnum::Disconnect(h.clone()),
            HandlerEnum::Recv(h) => HandlerEnum::Recv(h.clone()),
            HandlerEnum::SendAck(h) => HandlerEnum::SendAck(h.clone()),
        }
    }
}

impl ClonableMessageHandler for HandlerEnum {}

/// 用于保存消息处理器
pub type MessageHandlerMap = std::collections::HashMap<MessageType, HandlerEnum>; // 将值类型更新为 HandlerEnum