use msgtrans::compression::CompressionMethod;
use privchat_protocol::message::MessageType;
use privchat_protocol::message::*;
use privchat_sdk::handlers::{ConnectAckHandler, RecvHandler};
use privchat_sdk::PrivchatSDK;
use std::ffi::CStr;
use std::os::raw::c_char;

/// 创建一个新的 PrivchatSDK 实例，并注册默认的消息处理器。
///
/// # 参数
/// - `address`: 服务器地址
/// - `port`: 服务器端口
/// - `cert_path`: SSL 证书路径
///
/// # 返回值
/// - `*mut PrivchatSDK`: PrivchatSDK 实例的指针
#[no_mangle]
pub extern "C" fn privchat_sdk_new(
    address: *const c_char,
    port: u16,
    cert_path: *const c_char,
) -> *mut PrivchatSDK {
    let address = unsafe { CStr::from_ptr(address) }
        .to_str()
        .unwrap_or("127.0.0.1")
        .to_string();
    let cert_path = unsafe { CStr::from_ptr(cert_path) }
        .to_str()
        .unwrap_or("/path/to/cert.pem")
        .to_string();
    let mut sdk = PrivchatSDK::new(&address, port, cert_path);

    sdk.register_message_handler(MessageType::ConnectAck, ConnectAckHandler);
    sdk.register_message_handler(MessageType::Recv, RecvHandler);

    Box::into_raw(Box::new(sdk))
}

/// 连接到服务器。
///
/// # 参数
/// - `sdk_ptr`: PrivchatSDK 实例的指针
///
/// # 返回值
/// - `bool`: 连接是否成功
#[no_mangle]
pub extern "C" fn privchat_sdk_connect(sdk_ptr: *mut PrivchatSDK) -> bool {
    if sdk_ptr.is_null() {
        return false;
    }
    let sdk = unsafe { &mut *sdk_ptr };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime")
        .block_on(async { sdk.connect().await.is_ok() })
}

/// 发送消息。
///
/// # 参数
/// - `sdk_ptr`: PrivchatSDK 实例的指针
/// - `client_seq`: 客户端序列号
/// - `client_msg_no`: 客户端唯一消息编号
/// - `stream_no`: 流式编号
/// - `channel_id`: 频道ID
/// - `channel_type`: 频道类型
/// - `expire`: 消息过期时间（可选）
/// - `from_uid`: 发送者UID
/// - `topic`: 主题（可选）
/// - `payload`: 消息负载
/// - `payload_len`: 负载长度
///
/// # 返回值
/// - `bool`: 消息是否成功发送
#[no_mangle]
pub extern "C" fn privchat_sdk_send_message(
    sdk_ptr: *mut PrivchatSDK,
    client_seq: u32,
    client_msg_no: *const c_char,
    stream_no: *const c_char,
    channel_id: *const c_char,
    channel_type: u8,
    expire: Option<u32>,
    from_uid: *const c_char,
    topic: Option<*const c_char>,
    payload: *const u8,
    payload_len: usize,
) -> bool {
    if sdk_ptr.is_null()
        || client_msg_no.is_null()
        || stream_no.is_null()
        || channel_id.is_null()
        || from_uid.is_null()
        || payload.is_null()
    {
        return false;
    }

    let sdk = unsafe { &mut *sdk_ptr };

    let client_msg_no = unsafe { CStr::from_ptr(client_msg_no) }
        .to_str()
        .unwrap_or("unique_msg_no")
        .to_string();
    let stream_no = unsafe { CStr::from_ptr(stream_no) }
        .to_str()
        .unwrap_or("stream_001")
        .to_string();
    let channel_id = unsafe { CStr::from_ptr(channel_id) }
        .to_str()
        .unwrap_or("channel_01")
        .to_string();
    let from_uid = unsafe { CStr::from_ptr(from_uid) }
        .to_str()
        .unwrap_or("default_uid")
        .to_string();

    let topic = if let Some(topic_ptr) = topic {
        Some(
            unsafe { CStr::from_ptr(topic_ptr) }
                .to_str()
                .unwrap_or("default_topic")
                .to_string(),
        )
    } else {
        None
    };

    let payload_data = unsafe { std::slice::from_raw_parts(payload, payload_len) };

    let message = SendMessage {
        setting: Setting::new(),
        client_seq,
        client_msg_no,
        stream_no,
        channel_id,
        channel_type,
        expire,
        from_uid,
        topic,
        payload: payload_data.to_vec(),
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime")
        .block_on(async { sdk.send_message(message).await.is_ok() })
}

/// 设置压缩类型。
///
/// # 参数
/// - `sdk_ptr`: PrivchatSDK 实例的指针
/// - `compression_type`: 压缩类型（0: None, 1: Zstd, 2: Zlib）
///
/// # 返回值
/// - `bool`: 设置是否成功
#[no_mangle]
pub extern "C" fn privchat_sdk_set_compression_type(
    sdk_ptr: *mut PrivchatSDK,
    compression_type: u32,
) -> bool {
    if sdk_ptr.is_null() {
        return false;
    }
    let sdk = unsafe { &mut *sdk_ptr };
    let compression_method = match compression_type {
        0 => CompressionMethod::None,
        1 => CompressionMethod::Zstd,
        2 => CompressionMethod::Zlib,
        _ => CompressionMethod::None,
    };
    sdk.set_compression_type(compression_method);
    true
}

/// 释放 PrivchatSDK 实例。
///
/// # 参数
/// - `sdk_ptr`: PrivchatSDK 实例的指针
#[no_mangle]
pub extern "C" fn privchat_sdk_free(sdk_ptr: *mut PrivchatSDK) {
    if !sdk_ptr.is_null() {
        unsafe {
            drop(Box::from_raw(sdk_ptr));
        }
    }
}