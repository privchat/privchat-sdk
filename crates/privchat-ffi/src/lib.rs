use std::ffi::CStr;
use std::os::raw::c_char;
use privchat_sdk::PrivchatClient;

// 全局运行时管理
lazy_static::lazy_static! {
    static ref TOKIO_RUNTIME: tokio::runtime::Runtime = 
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to build Tokio runtime");
}

/// 创建一个新的 PrivchatClient 实例
///
/// # 参数
/// - `work_dir`: 工作目录路径
///
/// # 返回值
/// - `*mut PrivchatClient`: PrivchatClient 实例的指针，失败时返回空指针
/// 
/// # 注意
/// 当前版本的FFI需要完整的msgtrans Transport实现才能正常工作
#[no_mangle]
pub extern "C" fn privchat_client_new(
    work_dir: *const c_char,
) -> *mut PrivchatClient {
    let work_dir_str = unsafe { 
        if work_dir.is_null() {
            "./privchat_data"
        } else {
            CStr::from_ptr(work_dir)
                .to_str()
                .unwrap_or("./privchat_data")
        }
    };

    // 创建客户端实例（这里需要根据实际的msgtrans API来实现）
    // 目前返回null指针，等待实际的Transport实现
    eprintln!("⚠️  FFI功能需要完整的Transport实现");
    eprintln!("📁 工作目录: {}", work_dir_str);
    eprintln!("💡 请参考 examples/main.rs 查看完整的使用示例");
    std::ptr::null_mut()

    // TODO: 完整实现需要以下步骤：
    // 1. 创建或获取 Transport 实例
    // 2. 调用 PrivchatClient::new(work_dir_str, transport).await
    // 3. 返回 Box::into_raw(Box::new(client))
    //
    // 示例代码：
    // let transport = Arc::new(transport_instance);
    // match TOKIO_RUNTIME.block_on(PrivchatClient::new(work_dir_str, transport)) {
    //     Ok(client) => Box::into_raw(Box::new(client)),
    //     Err(_) => std::ptr::null_mut(),
    // }
}

/// 连接到服务器并登录
///
/// # 参数
/// - `client_ptr`: PrivchatClient 实例的指针
/// - `login`: 登录凭证（手机号、用户名等）
/// - `token`: 认证令牌（密码、短信验证码等）
///
/// # 返回值
/// - `true`: 连接成功
/// - `false`: 连接失败
#[no_mangle]
pub extern "C" fn privchat_client_connect(
    client_ptr: *mut PrivchatClient,
    login: *const c_char,
    token: *const c_char,
) -> bool {
    if client_ptr.is_null() || login.is_null() || token.is_null() {
        eprintln!("❌ 参数不能为空");
        return false;
    }
    
    eprintln!("⚠️  connect() 功能需要完整的Transport实现");
    false

    // TODO: 完整实现
    // let client = unsafe { &mut *client_ptr };
    // let login_str = unsafe { CStr::from_ptr(login) }.to_str().unwrap_or("");
    // let token_str = unsafe { CStr::from_ptr(token) }.to_str().unwrap_or("");
    // 
    // TOKIO_RUNTIME.block_on(async {
    //     client.connect(login_str, token_str).await.is_ok()
    // })
}

/// 断开连接
///
/// # 参数
/// - `client_ptr`: PrivchatClient 实例的指针
/// - `reason`: 断开原因
///
/// # 返回值
/// - `true`: 断开成功
/// - `false`: 断开失败
#[no_mangle]
pub extern "C" fn privchat_client_disconnect(
    client_ptr: *mut PrivchatClient,
    _reason: *const c_char,
) -> bool {
    if client_ptr.is_null() {
        return false;
    }
    
    eprintln!("⚠️  disconnect() 功能需要完整的Transport实现");
    false

    // TODO: 完整实现
    // let client = unsafe { &mut *client_ptr };
    // let reason_str = if _reason.is_null() {
    //     "客户端主动断开"
    // } else {
    //     unsafe { CStr::from_ptr(_reason) }.to_str().unwrap_or("客户端主动断开")
    // };
    // 
    // TOKIO_RUNTIME.block_on(async {
    //     client.disconnect(reason_str).await.is_ok()
    // })
}

/// 获取当前用户ID
///
/// # 参数
/// - `client_ptr`: PrivchatClient 实例的指针
/// - `buffer`: 输出缓冲区
/// - `buffer_len`: 缓冲区长度
///
/// # 返回值
/// - `true`: 获取成功
/// - `false`: 获取失败
#[no_mangle]
pub extern "C" fn privchat_client_get_user_id(
    client_ptr: *mut PrivchatClient,
    buffer: *mut c_char,
    buffer_len: usize,
) -> bool {
    if client_ptr.is_null() || buffer.is_null() || buffer_len == 0 {
        return false;
    }
    
    eprintln!("⚠️  get_user_id() 功能需要完整的Transport实现");
    false

    // TODO: 完整实现
    // let client = unsafe { &*client_ptr };
    // if let Some(user_id) = client.user_id() {
    //     let user_id_bytes = user_id.as_bytes();
    //     if user_id_bytes.len() + 1 <= buffer_len {
    //         unsafe {
    //             std::ptr::copy_nonoverlapping(
    //                 user_id_bytes.as_ptr() as *const c_char,
    //                 buffer,
    //                 user_id_bytes.len(),
    //             );
    //             *buffer.add(user_id_bytes.len()) = 0; // null terminator
    //         }
    //         return true;
    //     }
    // }
    // false
}

/// 检查连接状态
///
/// # 参数
/// - `client_ptr`: PrivchatClient 实例的指针
///
/// # 返回值
/// - `true`: 已连接
/// - `false`: 未连接
#[no_mangle]
pub extern "C" fn privchat_client_is_connected(client_ptr: *mut PrivchatClient) -> bool {
    if client_ptr.is_null() {
        return false;
    }

    eprintln!("⚠️  is_connected() 功能需要完整的Transport实现");
    false

    // TODO: 完整实现
    // let client = unsafe { &*client_ptr };
    // TOKIO_RUNTIME.block_on(async {
    //     client.is_connected().await
    // })
}

/// 释放 PrivchatClient 实例
///
/// # 参数
/// - `client_ptr`: PrivchatClient 实例的指针
#[no_mangle]
pub extern "C" fn privchat_client_free(client_ptr: *mut PrivchatClient) {
    if !client_ptr.is_null() {
        unsafe {
            let _ = Box::from_raw(client_ptr);
        }
    }
}

/// 获取FFI版本信息
///
/// # 参数
/// - `buffer`: 输出缓冲区
/// - `buffer_len`: 缓冲区长度
///
/// # 返回值
/// - `true`: 获取成功
/// - `false`: 缓冲区太小
#[no_mangle]
pub extern "C" fn privchat_get_version(
    buffer: *mut c_char,
    buffer_len: usize,
) -> bool {
    if buffer.is_null() || buffer_len == 0 {
        return false;
    }

    let version = "PrivchatSDK v0.1.0 - Rust FFI";
    let version_bytes = version.as_bytes();
    
    if version_bytes.len() + 1 <= buffer_len {
        unsafe {
            std::ptr::copy_nonoverlapping(
                version_bytes.as_ptr() as *const c_char,
                buffer,
                version_bytes.len(),
            );
            *buffer.add(version_bytes.len()) = 0; // null terminator
        }
        true
    } else {
        false
    }
}