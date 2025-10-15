// src/ffi.rs

use crate::base::app::AppVisitor;
use crate::impls::apps::wechat::jwqywx::JwqywxApplication;
use crate::impls::client::DefaultClient;
use crate::impls::login::sso::SSOUniversalLogin;
use crate::utils::status::services_status_code;
use libc::c_char;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::ffi::{CStr, CString};
use tokio::runtime::Runtime;

// 1. 全局 Tokio 运行时
// 创建一个全局的 Tokio 运行时来执行所有的异步代码。
static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime")
});

// 2. FFI 结果封装
// 定义一个通用的返回结构体，用于将成功或失败的结果序列化为 JSON。
#[derive(Serialize)]
struct FfiResult<T: Serialize> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

impl<T: Serialize> FfiResult<T> {
    fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    fn error(msg: &str) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.to_string()),
        }
    }

    fn to_json_string(self) -> String {
        serde_json::to_string(&self).unwrap_or_else(|e| {
            serde_json::to_string(&FfiResult::<()>::error(&format!(
                "JSON serialization failed: {}",
                e
            )))
            .unwrap()
        })
    }
}

// 3. 客户端管理函数

/// 创建一个新的 cczuni 客户端实例。
///
/// # Arguments
/// * `user` - C 字符串，用户的学号。
/// * `password` - C 字符串，用户的密码。
///
/// # Returns
/// 返回一个指向客户端实例的不透明指针。如果创建失败，返回空指针。
/// **调用者必须在使用完毕后调用 `cczuni_client_free` 来释放内存。**
#[no_mangle]
pub extern "C" fn cczuni_client_new(
    user: *const c_char,
    password: *const c_char,
) -> *mut DefaultClient {
    let user_str = unsafe { CStr::from_ptr(user).to_string_lossy().into_owned() };
    let password_str = unsafe { CStr::from_ptr(password).to_string_lossy().into_owned() };

    let client = DefaultClient::account(user_str, password_str);
    Box::into_raw(Box::new(client))
}

/// 释放 cczuni 客户端实例占用的内存。
///
/// # Arguments
/// * `client_ptr` - 通过 `cczuni_client_new` 创建的客户端指针。
#[no_mangle]
pub extern "C" fn cczuni_client_free(client_ptr: *mut DefaultClient) {
    if !client_ptr.is_null() {
        unsafe {
            let _ = Box::from_raw(client_ptr);
        }
    }
}

// 4. 核心功能函数

/// 使用指定的客户端进行统一身份认证登录。
///
/// # Arguments
/// * `client_ptr` - 客户端指针。
///
/// # Returns
/// 返回一个 JSON 字符串，包含登录结果。
/// **返回的字符串必须使用 `cczuni_free_string` 进行释放。**
#[no_mangle]
pub extern "C" fn cczuni_login(client_ptr: *mut DefaultClient) -> *mut c_char {
    let client = unsafe { &*client_ptr };

    let result_json = RUNTIME.block_on(async {
        match client.sso_universal_login().await {
            Ok(login_info) => FfiResult::success(login_info).to_json_string(),
            Err(e) => FfiResult::<()>::error(&e.to_string()).to_json_string(),
        }
    });

    CString::new(result_json).unwrap().into_raw()
}

/// 获取学生的成绩列表。
///
/// # Arguments
/// * `client_ptr` - **已登录的**客户端指针。
///
/// # Returns
/// 返回一个包含成绩信息的 JSON 字符串。
/// **返回的字符串必须使用 `cczuni_free_string` 进行释放。**
#[no_mangle]
pub extern "C" fn cczuni_get_grades(client_ptr: *mut DefaultClient) -> *mut c_char {
    let client = unsafe { &*client_ptr };

    let result_json = RUNTIME.block_on(async {
        // 我们使用 JwqywxApplication 作为示例，因为它返回结构化的数据
        let app = client.visit::<JwqywxApplication<_>>().await;

        // Jwqywx 需要先执行自己的登录
        if let Err(e) = app.login().await {
            return FfiResult::<()>::error(&format!("Failed to login to Jwqywx: {}", e))
                .to_json_string();
        }

        match app.get_grades().await {
            Ok(grades_msg) => FfiResult::success(grades_msg.message).to_json_string(),
            Err(e) => FfiResult::<()>::error(&e.to_string()).to_json_string(),
        }
    });

    CString::new(result_json).unwrap().into_raw()
}

/// 获取学生的课表信息。
///
/// # Arguments
/// * `client_ptr` - **已登录的**客户端指针。
///
/// # Returns
/// 返回一个包含课表信息的 JSON 字符串。
/// **返回的字符串必须使用 `cczuni_free_string` 进行释放。**
#[no_mangle]
pub extern "C" fn cczuni_get_schedule(client_ptr: *mut DefaultClient) -> *mut c_char {
    let client = unsafe { &*client_ptr };

    let result_json = RUNTIME.block_on(async {
        let app = client.visit::<JwqywxApplication<_>>().await;

        if let Err(e) = app.login().await {
            return FfiResult::<()>::error(&format!("Failed to login to Jwqywx: {}", e))
                .to_json_string();
        }

        match app.terms().await {
            Ok(terms) => {
                if let Some(current_term) = terms.message.first() {
                    // 导入 TermCalendarParser trait
                    use crate::extension::calendar::TermCalendarParser;
                    match app
                        .get_term_classinfo_week_matrix(current_term.term.clone())
                        .await
                    {
                        Ok(matrix) => FfiResult::success(matrix).to_json_string(),
                        Err(e) => FfiResult::<()>::error(&e.to_string()).to_json_string(),
                    }
                } else {
                    FfiResult::<()>::error("No terms found").to_json_string()
                }
            }
            Err(e) => FfiResult::<()>::error(&e.to_string()).to_json_string(),
        }
    });

    CString::new(result_json).unwrap().into_raw()
}

/// 获取各个服务的在线状态。
///
/// # Returns
/// 返回一个包含服务状态的 JSON 字符串。
/// **返回的字符串必须使用 `cczuni_free_string` 进行释放。**
#[no_mangle]
pub extern "C" fn cczuni_get_services_status() -> *mut c_char {
    let result_json = RUNTIME.block_on(async {
        let status_map = services_status_code().await;
        // 将 StatusCode 转换为 u16 数字以便序列化
        let serializable_map: std::collections::HashMap<_, _> = status_map
            .into_iter()
            .map(|(k, v)| (k, v.as_u16()))
            .collect();
        FfiResult::success(serializable_map).to_json_string()
    });

    CString::new(result_json).unwrap().into_raw()
}

// 5. 内存管理

/// 释放由 cczuni 库函数返回的字符串所占用的内存。
///
/// # Arguments
/// * `string_ptr` - 指向由其他 `cczuni_` 函数返回的 C 字符串的指针。
#[no_mangle]
pub extern "C" fn cczuni_free_string(string_ptr: *mut c_char) {
    if !string_ptr.is_null() {
        unsafe {
            let _ = CString::from_raw(string_ptr);
        }
    }
}
