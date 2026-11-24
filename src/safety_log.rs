/// Messages contain operation names, counts and OS error codes. Keep user paths private.
pub fn event(error: bool, message: &str) {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        use std::ffi::{CString, c_char, c_int};
        unsafe extern "C" {
            fn macsweep_log(error: c_int, message: *const c_char);
        }
        if let Ok(message) = CString::new(message) {
            // The C function reads the string only during this call.
            unsafe { macsweep_log(i32::from(error), message.as_ptr()) };
        }
    }
    #[cfg(any(not(target_os = "macos"), test))]
    println!("MacSweep [error={error}] {message}");
}
