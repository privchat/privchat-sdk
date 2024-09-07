use super::message::{Message};
use privchat_protocol::message::*;

pub trait CloneBox {
    fn clone_box(&self) -> Box<dyn MessageHandler>;
}

impl<T> CloneBox for T
where
    T: 'static + MessageHandler + Clone,
{
    fn clone_box(&self) -> Box<dyn MessageHandler> {
        Box::new(self.clone())
    }
}

pub trait MessageHandler: Send + Sync + CloneBox {
    fn handle_message(&self, message: Box<dyn Message>);
}

impl Clone for Box<dyn MessageHandler> {
    fn clone(&self) -> Box<dyn MessageHandler> {
        self.clone_box()
    }
}

// TODO: 根据实际业务情况进行调整
#[derive(Clone)]
pub struct ConnectAckHandler;

impl MessageHandler for ConnectAckHandler {
    fn handle_message(&self, message: Box<dyn Message>) {
        if let Some(connect_ack) = message.as_any().downcast_ref::<ConnectAckMessage>() {
            println!("Handling ConnectAckMessage: {:?}", connect_ack);
        }
    }
}

#[derive(Clone)]
pub struct DisconnectHandler;

impl MessageHandler for DisconnectHandler {
    fn handle_message(&self, message: Box<dyn Message>) {
        if let Some(disconnect) = message.as_any().downcast_ref::<DisconnectMessage>() {
            println!("Handling DisconnectMessage: {:?}", disconnect);
        }
    }
}

#[derive(Clone)]
pub struct RecvHandler;

impl MessageHandler for RecvHandler {
    fn handle_message(&self, message: Box<dyn Message>) {
        if let Some(recv) = message.as_any().downcast_ref::<RecvMessage>() {
            println!("Handling RecvMessage: {:?}", recv);
        }
    }
}

#[derive(Clone)]
pub struct SendAckHandler;

impl MessageHandler for SendAckHandler {
    fn handle_message(&self, message: Box<dyn Message>) {
        if let Some(send_ack) = message.as_any().downcast_ref::<SendAckMessage>() {
            println!("Handling SendAckMessage: {:?}", send_ack);
        }
    }
}