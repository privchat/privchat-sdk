use libc::{c_char, c_void, size_t, uint32_t, uint64_t};
use std::ffi::CString;

// 定义 C 字符串转换宏
macro_rules! cstr {
    ($s:expr) => {
        CString::new($s).expect("CString::new failed")
    };
}

// 定义 C 字符串指针转换宏
macro_rules! cstr_ptr {
    ($s:expr) => {
        cstr!($s).as_ptr()
    };
}

// 包含 FFI 函数声明
extern "C" {
    fn privchat_sdk_new(
        address: *const c_char,
        port: u16,
        cert_path: *const c_char,
    ) -> *mut privchat_sdk::PrivchatSDK;

    fn privchat_sdk_connect(sdk_ptr: *mut privchat_sdk::PrivchatSDK) -> bool;
    
    fn privchat_sdk_disconnect(sdk_ptr: *mut privchat_sdk::PrivchatSDK) -> bool;

    fn privchat_sdk_send_chat_message(
        sdk_ptr: *mut privchat_sdk::PrivchatSDK,
        client_seq: u32,
        client_msg_no: *const c_char,
        stream_no: *const c_char,
        channel_id: *const c_char,
        channel_type: u8,
        expire: uint64_t,
        from_uid: *const c_char,
        topic: *const c_char,
        payload: *const u8,
        payload_len: size_t,
    ) -> bool;

    fn privchat_sdk_subscribe_channel(
        sdk_ptr: *mut privchat_sdk::PrivchatSDK,
        channel_id: *const c_char,
    ) -> bool;

    fn privchat_sdk_send_ping(sdk_ptr: *mut privchat_sdk::PrivchatSDK) -> bool;

    fn privchat_sdk_is_connected(sdk_ptr: *mut privchat_sdk::PrivchatSDK) -> bool;

    fn privchat_sdk_set_compression_type(
        sdk_ptr: *mut privchat_sdk::PrivchatSDK,
        compression_type: uint32_t,
    ) -> bool;

    fn privchat_sdk_get_stats(
        sdk_ptr: *mut privchat_sdk::PrivchatSDK,
        buffer: *mut c_char,
        buffer_len: size_t,
    ) -> bool;

    fn privchat_sdk_free(sdk_ptr: *mut privchat_sdk::PrivchatSDK);
}

// 测试用例
#[test]
fn test_privchat_sdk_new_and_free() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}

#[test]
fn test_privchat_sdk_connect() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    // 注意：这里测试连接可能会失败，因为没有实际的服务器
    // 在实际测试中，应该先启动测试服务器
    let _connected = unsafe { privchat_sdk_connect(sdk_ptr) };
    // assert!(connected, "Connection failed");

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}

#[test]
fn test_privchat_sdk_send_chat_message() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    // 尝试连接（可能失败）
    let _connected = unsafe { privchat_sdk_connect(sdk_ptr) };

    let client_seq = 12345;
    let client_msg_no = cstr_ptr!("msg_001");
    let stream_no = cstr_ptr!("stream_001");
    let channel_id = cstr_ptr!("general");
    let channel_type = 1;
    let expire = 3600;
    let from_uid = cstr_ptr!("user_123");
    let topic = cstr_ptr!("问候");
    let payload = b"Hello, PrivChat!";
    let payload_len = payload.len();

    let _sent = unsafe {
        privchat_sdk_send_chat_message(
            sdk_ptr,
            client_seq,
            client_msg_no,
            stream_no,
            channel_id,
            channel_type,
            expire,
            from_uid,
            topic,
            payload.as_ptr(),
            payload_len,
        )
    };
    // assert!(sent, "Message sending failed");

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}

#[test]
fn test_privchat_sdk_subscribe_channel() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    let channel_id = cstr_ptr!("general");
    let _subscribed = unsafe { privchat_sdk_subscribe_channel(sdk_ptr, channel_id) };

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}

#[test]
fn test_privchat_sdk_send_ping() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    let _pinged = unsafe { privchat_sdk_send_ping(sdk_ptr) };

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}

#[test]
fn test_privchat_sdk_is_connected() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    let connected = unsafe { privchat_sdk_is_connected(sdk_ptr) };
    assert!(!connected, "Should not be connected initially");

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}

#[test]
fn test_privchat_sdk_set_compression_type() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    let compression_type = 1; // Zstd
    let set = unsafe { privchat_sdk_set_compression_type(sdk_ptr, compression_type) };
    assert!(set, "Compression type setting failed");

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}

#[test]
fn test_privchat_sdk_get_stats() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    let mut buffer = [0i8; 256];
    let success = unsafe { privchat_sdk_get_stats(sdk_ptr, buffer.as_mut_ptr(), buffer.len()) };
    assert!(success, "Get stats failed");

    // 将C字符串转换为Rust字符串验证
    let c_str = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    let stats = c_str.to_str().expect("Invalid UTF-8 in stats");
    assert!(!stats.is_empty(), "Stats should not be empty");
    println!("SDK Stats: {}", stats);

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}