use libc::{c_char, c_void, size_t, uint32_t};

// 定义 C 字符串转换宏
macro_rules! cstr {
    ($s:expr) => {
        std::ffi::CString::new($s).expect("CString::new failed")
    };
}

// 定义 C 字符串指针转换宏
macro_rules! cstr_ptr {
    ($s:expr) => {
        cstr!($s).as_ptr()
    };
}

// 定义 C 字节切片转换宏
macro_rules! c_byte_slice {
    ($bytes:expr) => {
        Box::leak(Box::new($bytes.to_vec()))
    };
}

// 包含 FFI 函数声明
#[link(name = "privchat_ffi")]
extern "C" {
    fn privchat_sdk_new(
        address: *const c_char,
        port: u16,
        cert_path: *const c_char,
    ) -> *mut privchat_sdk::PrivchatSDK;

    fn privchat_sdk_connect(sdk_ptr: *mut privchat_sdk::PrivchatSDK) -> bool;

    fn privchat_sdk_send_message(
        sdk_ptr: *mut privchat_sdk::PrivchatSDK,
        client_seq: u32,
        client_msg_no: *const c_char,
        stream_no: *const c_char,
        channel_id: *const c_char,
        channel_type: u8,
        expire: Option<uint32_t>,
        from_uid: *const c_char,
        topic: Option<*const c_char>,
        payload: *const u8,
        payload_len: size_t,
    ) -> bool;

    fn privchat_sdk_set_compression_type(
        sdk_ptr: *mut privchat_sdk::PrivchatSDK,
        compression_type: uint32_t,
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

    let connected = unsafe { privchat_sdk_connect(sdk_ptr) };
    assert!(connected, "Connection failed");

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}

#[test]
fn test_privchat_sdk_send_message() {
    let address = cstr_ptr!("127.0.0.1");
    let port = 8080;
    let cert_path = cstr_ptr!("/path/to/cert.pem");

    let sdk_ptr = unsafe { privchat_sdk_new(address, port, cert_path) };
    assert!(!sdk_ptr.is_null(), "SDK creation failed");

    let connected = unsafe { privchat_sdk_connect(sdk_ptr) };
    assert!(connected, "Connection failed");

    let client_seq = 12345;
    let client_msg_no = cstr_ptr!("unique_msg_no");
    let stream_no = cstr_ptr!("stream_001");
    let channel_id = cstr_ptr!("channel_01");
    let channel_type = 1;
    let expire = Some(300);
    let from_uid = cstr_ptr!("default_uid");
    let topic = Some(cstr_ptr!("default_topic"));
    let payload = c_byte_slice!([1, 2, 3, 4, 5]);
    let payload_len = 5;

    let sent = unsafe {
        privchat_sdk_send_message(
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
    assert!(sent, "Message sending failed");

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

    let connected = unsafe { privchat_sdk_connect(sdk_ptr) };
    assert!(connected, "Connection failed");

    let compression_type = 1; // Zstd
    let set = unsafe { privchat_sdk_set_compression_type(sdk_ptr, compression_type) };
    assert!(set, "Compression type setting failed");

    // 清理
    unsafe { privchat_sdk_free(sdk_ptr) };
}