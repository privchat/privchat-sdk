use privchat_sdk::PrivchatSDK;
use std::ffi::CStr;
use std::os::raw::c_char;

// FFI 函数定义
#[no_mangle]
pub extern "C" fn privchat_sdk_new(address: *const c_char, port: u16, cert_path: *const c_char) -> *mut PrivchatSDK {
    let address = unsafe { CStr::from_ptr(address) }.to_str().unwrap_or("127.0.0.1").to_string();
    let cert_path = unsafe { CStr::from_ptr(cert_path) }.to_str().unwrap_or("/path/to/cert.pem").to_string();
    let sdk = PrivchatSDK::new(&address, port, cert_path);
    Box::into_raw(Box::new(sdk))
}

#[no_mangle]
pub extern "C" fn privchat_sdk_connect(sdk_ptr: *mut PrivchatSDK) -> bool {
    let sdk = unsafe { &mut *sdk_ptr };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        sdk.connect().await.is_ok()
    })
}

#[no_mangle]
pub extern "C" fn privchat_sdk_free(sdk_ptr: *mut PrivchatSDK) {
    unsafe {
        drop(Box::from_raw(sdk_ptr));
    }
}