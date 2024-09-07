use super::*;

#[test]
fn test_privchat_sdk_new_and_connect() {
    let address = "127.0.0.1";
    let port = 8080;
    let cert_path = "/path/to/cert.pem";

    let address_cstr = CString::new(address).unwrap();
    let cert_path_cstr = CString::new(cert_path).unwrap();

    let sdk_ptr = unsafe {
        privchat_sdk_new(
            address_cstr.as_ptr(),
            port,
            cert_path_cstr.as_ptr(),
        )
    };

    assert!(!sdk_ptr.is_null(), "SDK pointer should not be null");

    let connected = unsafe {
        privchat_sdk_connect(sdk_ptr)
    };

    assert!(connected, "SDK should connect successfully");

    // 释放 SDK 实例
    unsafe {
        privchat_sdk_free(sdk_ptr);
    }
}