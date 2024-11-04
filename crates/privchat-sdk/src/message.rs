use std::any::Any;
use privchat_protocol::message::MessageType;

pub trait Message {
    fn get_message_type(&self) -> MessageType;
    fn as_any(&self) -> &dyn Any;
}
