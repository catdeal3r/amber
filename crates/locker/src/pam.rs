//! Minimal binding to the Linux-PAM client API.
//!
//! Only the handful of `libpam` entry points needed to verify a password are
//! declared here, so the locker links against the system `libpam` without
//! pulling in `pam-sys` (which generates bindings with `bindgen` at build
//! time).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;

const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;

const PAM_SILENT: c_int = 0x8000;
const PAM_DISALLOW_NULL_AUTHTOK: c_int = 0x0001;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type Conversation = unsafe extern "C" fn(
    num_msg: c_int,
    msg: *const *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Option<Conversation>,
    appdata: *mut c_void,
}

/// Opaque `pam_handle_t`.
enum PamHandle {}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        handle: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(handle: *mut PamHandle, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut PamHandle, errnum: c_int) -> *const c_char;

    fn calloc(nmemb: usize, size: usize) -> *mut c_void;
    fn strdup(s: *const c_char) -> *mut c_char;
}

/// Credentials handed to the PAM conversation callback through `appdata`.
struct Credentials {
    user: CString,
    password: CString,
}

/// Answers PAM's prompts: the username for echo-on prompts and the password
/// for echo-off prompts, leaving informational messages unanswered.
///
/// # Safety
/// Called by `libpam` with a valid message array and the `Credentials` pointer
/// passed to [`pam_start`].
unsafe extern "C" fn converse(
    num_msg: c_int,
    msg: *const *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    unsafe {
        if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata.is_null() {
            return PAM_CONV_ERR;
        }

        let credentials = &*appdata.cast::<Credentials>();
        let count = num_msg as usize;

        // PAM frees the reply (and each response string) with `free`, so it
        // must be allocated with the C allocator.
        let responses = calloc(count, size_of::<PamResponse>()).cast::<PamResponse>();
        if responses.is_null() {
            return PAM_BUF_ERR;
        }

        for index in 0..count {
            let message = *msg.add(index);
            let style = if message.is_null() {
                0
            } else {
                (*message).msg_style
            };

            let response = &mut *responses.add(index);
            response.resp_retcode = 0;
            response.resp = match style {
                PAM_PROMPT_ECHO_OFF => strdup(credentials.password.as_ptr()),
                PAM_PROMPT_ECHO_ON => strdup(credentials.user.as_ptr()),
                _ => ptr::null_mut(),
            };
        }

        *resp = responses;
        PAM_SUCCESS
    }
}

/// Verify `password` for `user` against PAM service `service`.
///
/// Blocking: run it off the UI thread (PAM may talk to the network or a smart
/// card). Returns a human-readable message on failure.
pub fn authenticate(service: &str, user: &str, password: &str) -> Result<(), String> {
    let service = CString::new(service).map_err(|_| "invalid PAM service name".to_owned())?;
    let credentials = Credentials {
        user: CString::new(user).map_err(|_| "invalid user name".to_owned())?,
        password: CString::new(password).map_err(|_| "invalid password".to_owned())?,
    };
    let conv = PamConv {
        conv: Some(converse),
        appdata: ptr::from_ref(&credentials).cast_mut().cast::<c_void>(),
    };

    let mut handle: *mut PamHandle = ptr::null_mut();
    let started = unsafe {
        pam_start(
            service.as_ptr(),
            credentials.user.as_ptr(),
            &conv,
            &mut handle,
        )
    };
    if started != PAM_SUCCESS {
        return Err(message(ptr::null_mut(), started));
    }

    let result = unsafe {
        let auth = pam_authenticate(handle, PAM_DISALLOW_NULL_AUTHTOK);
        if auth != PAM_SUCCESS {
            Err(message(handle, auth))
        } else {
            let account = pam_acct_mgmt(handle, PAM_SILENT);
            if account != PAM_SUCCESS {
                Err(message(handle, account))
            } else {
                Ok(())
            }
        }
    };

    unsafe {
        pam_end(handle, PAM_SUCCESS);
    }

    result
}

fn message(handle: *mut PamHandle, code: c_int) -> String {
    let text = unsafe { pam_strerror(handle, code) };
    if text.is_null() {
        format!("authentication error ({code})")
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}
