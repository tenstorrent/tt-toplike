#![feature(prelude_import)]
//! FFI bindings to the wayland system libraries.
//!
//! The names exported by this crate should *not* be used directly, but through
//! the `ffi_dispatch` macro, like this:
//!
//! ```ignore
//! ffi_dispatch!(HANDLE_NAME, func_name, arg1, arg2, arg3);
//! ```
//!
//! Where `HANDLE_NAME` is the name of the handle generated if the cargo feature `dlopen` is on.
//!
//! For this to work, you must ensure every needed symbol is in scope (aka the static handle
//! if `dlopen` is on, the extern function if not). The easiest way to do this is to glob import
//! the appropriate module. For example:
//!
//! ```ignore
//! #[macro_use] extern crate wayland_sys;
//!
//! use wayland_sys::client::*;
//!
//! let display_ptr = unsafe {
//!         ffi_dispatch!(wayland_client_handle(), wl_display_connect, ::std::ptr::null())
//! };
//! ```
//!
//! Each module except `common` corresponds to a system library. They all define a function named
//! `is_lib_available()` which returns whether the library could be loaded. They always return true
//! if the feature `dlopen` is absent, as we link against the library directly in that case.
#![allow(non_camel_case_types)]
#![forbid(improper_ctypes, unsafe_op_in_unsafe_fn)]
#[macro_use]
extern crate std;
#[prelude_import]
use std::prelude::rust_2021::*;
#[allow(unused_imports)]
#[macro_use]
extern crate dlib;
pub mod common {
    //! Various types and functions that are used by both the client and the server
    //! libraries.
    use std::os::unix::io::RawFd;
    use std::{fmt, os::raw::{c_char, c_int, c_void}};
    #[repr(C)]
    pub struct wl_message {
        pub name: *const c_char,
        pub signature: *const c_char,
        pub types: *const *const wl_interface,
    }
    #[repr(C)]
    pub struct wl_interface {
        pub name: *const c_char,
        pub version: c_int,
        pub request_count: c_int,
        pub requests: *const wl_message,
        pub event_count: c_int,
        pub events: *const wl_message,
    }
    impl fmt::Debug for wl_interface {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_fmt(format_args!("wl_interface@{0:p}", self))
        }
    }
    unsafe impl Send for wl_interface {}
    unsafe impl Sync for wl_interface {}
    #[repr(C)]
    pub struct wl_list {
        pub prev: *mut wl_list,
        pub next: *mut wl_list,
    }
    #[repr(C)]
    pub struct wl_array {
        pub size: usize,
        pub alloc: usize,
        pub data: *mut c_void,
    }
    pub type wl_fixed_t = i32;
    pub fn wl_fixed_to_double(f: wl_fixed_t) -> f64 {
        f64::from(f) / 256.
    }
    pub fn wl_fixed_from_double(d: f64) -> wl_fixed_t {
        (d * 256.) as i32
    }
    pub fn wl_fixed_to_int(f: wl_fixed_t) -> i32 {
        f / 256
    }
    pub fn wl_fixed_from_int(i: i32) -> wl_fixed_t {
        i * 256
    }
    #[repr(C)]
    pub union wl_argument {
        pub i: i32,
        pub u: u32,
        pub f: wl_fixed_t,
        pub s: *const c_char,
        pub o: *const c_void,
        pub n: u32,
        pub a: *const wl_array,
        pub h: RawFd,
    }
    pub type wl_dispatcher_func_t = unsafe extern "C" fn(
        *const c_void,
        *mut c_void,
        u32,
        *const wl_message,
        *const wl_argument,
    ) -> c_int;
    pub type wl_log_func_t = unsafe extern "C" fn(*const c_char, *const c_void);
}
pub mod client {
    //! Bindings to the client library `libwayland-client.so`
    //!
    //! The generated handle is named `wayland_client_handle()`
    use once_cell::sync::Lazy;
    use super::common::*;
    use std::os::raw::{c_char, c_int, c_void};
    pub enum wl_proxy {}
    pub enum wl_display {}
    pub enum wl_event_queue {}
    pub struct WaylandClient {
        __lib: ::dlib::Library,
        pub wl_display_connect_to_fd: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(c_int) -> *mut wl_display,
        >,
        pub wl_display_connect: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*const c_char) -> *mut wl_display,
        >,
        pub wl_display_disconnect: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> (),
        >,
        pub wl_display_get_fd: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_display_roundtrip: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_display_read_events: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_display_prepare_read: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_display_cancel_read: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> (),
        >,
        pub wl_display_dispatch: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_display_dispatch_pending: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_display_get_error: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_display_get_protocol_error: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_display,
                *mut *const wl_interface,
                *mut u32,
            ) -> u32,
        >,
        pub wl_display_flush: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_event_queue_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_queue) -> (),
        >,
        pub wl_display_create_queue: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> *mut wl_event_queue,
        >,
        pub wl_display_roundtrip_queue: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, *mut wl_event_queue) -> c_int,
        >,
        pub wl_display_prepare_read_queue: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, *mut wl_event_queue) -> c_int,
        >,
        pub wl_display_dispatch_queue: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, *mut wl_event_queue) -> c_int,
        >,
        pub wl_display_dispatch_queue_pending: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, *mut wl_event_queue) -> c_int,
        >,
        pub wl_proxy_create: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy, *const wl_interface) -> *mut wl_proxy,
        >,
        pub wl_proxy_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy) -> (),
        >,
        pub wl_proxy_add_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_proxy,
                *mut extern "C" fn(),
                *mut c_void,
            ) -> c_int,
        >,
        pub wl_proxy_get_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy) -> *const c_void,
        >,
        pub wl_proxy_add_dispatcher: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_proxy,
                wl_dispatcher_func_t,
                *const c_void,
                *mut c_void,
            ) -> c_int,
        >,
        pub wl_proxy_marshal_array_constructor: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_proxy,
                u32,
                *mut wl_argument,
                *const wl_interface,
            ) -> *mut wl_proxy,
        >,
        pub wl_proxy_marshal_array_constructor_versioned: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_proxy,
                u32,
                *mut wl_argument,
                *const wl_interface,
                u32,
            ) -> *mut wl_proxy,
        >,
        pub wl_proxy_marshal_array: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy, u32, *mut wl_argument) -> (),
        >,
        pub wl_proxy_set_user_data: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy, *mut c_void) -> (),
        >,
        pub wl_proxy_get_user_data: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy) -> *mut c_void,
        >,
        pub wl_proxy_get_id: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy) -> u32,
        >,
        pub wl_proxy_get_class: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy) -> *const c_char,
        >,
        pub wl_proxy_set_queue: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy, *mut wl_event_queue) -> (),
        >,
        pub wl_proxy_get_version: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy) -> u32,
        >,
        pub wl_proxy_create_wrapper: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy) -> *mut wl_proxy,
        >,
        pub wl_proxy_wrapper_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy) -> (),
        >,
        pub wl_log_set_handler_client: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(wl_log_func_t) -> (),
        >,
        pub wl_list_init: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list) -> (),
        >,
        pub wl_list_insert: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list, *mut wl_list) -> (),
        >,
        pub wl_list_remove: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list) -> (),
        >,
        pub wl_list_length: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*const wl_list) -> c_int,
        >,
        pub wl_list_empty: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*const wl_list) -> c_int,
        >,
        pub wl_list_insert_list: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list, *mut wl_list) -> (),
        >,
        pub wl_array_init: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_array) -> (),
        >,
        pub wl_array_release: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_array) -> (),
        >,
        pub wl_array_add: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_array, usize) -> (),
        >,
        pub wl_array_copy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_array, *mut wl_array) -> (),
        >,
        pub wl_proxy_marshal_constructor: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_proxy,
                u32,
                *const wl_interface,
                ...
            ) -> *mut wl_proxy,
        >,
        pub wl_proxy_marshal_constructor_versioned: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_proxy,
                u32,
                *const wl_interface,
                u32,
                ...
            ) -> *mut wl_proxy,
        >,
        pub wl_proxy_marshal: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy, u32, ...) -> (),
        >,
    }
    impl WaylandClient {
        pub unsafe fn open(name: &str) -> Result<WaylandClient, ::dlib::DlError> {
            use std::mem::transmute;
            let lib = ::dlib::Library::new(name).map_err(::dlib::DlError::CantOpen)?;
            let s = WaylandClient {
                wl_display_connect_to_fd: {
                    let s_name = "wl_display_connect_to_fd\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(c_int) -> *mut wl_display,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_connect: {
                    let s_name = "wl_display_connect\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*const c_char) -> *mut wl_display,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_disconnect: {
                    let s_name = "wl_display_disconnect\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_get_fd: {
                    let s_name = "wl_display_get_fd\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_roundtrip: {
                    let s_name = "wl_display_roundtrip\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_read_events: {
                    let s_name = "wl_display_read_events\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_prepare_read: {
                    let s_name = "wl_display_prepare_read\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_cancel_read: {
                    let s_name = "wl_display_cancel_read\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_dispatch: {
                    let s_name = "wl_display_dispatch\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_dispatch_pending: {
                    let s_name = "wl_display_dispatch_pending\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_get_error: {
                    let s_name = "wl_display_get_error\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_get_protocol_error: {
                    let s_name = "wl_display_get_protocol_error\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *mut *const wl_interface,
                                    *mut u32,
                                ) -> u32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_flush: {
                    let s_name = "wl_display_flush\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_queue_destroy: {
                    let s_name = "wl_event_queue_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_queue) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_create_queue: {
                    let s_name = "wl_display_create_queue\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> *mut wl_event_queue,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_roundtrip_queue: {
                    let s_name = "wl_display_roundtrip_queue\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *mut wl_event_queue,
                                ) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_prepare_read_queue: {
                    let s_name = "wl_display_prepare_read_queue\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *mut wl_event_queue,
                                ) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_dispatch_queue: {
                    let s_name = "wl_display_dispatch_queue\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *mut wl_event_queue,
                                ) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_dispatch_queue_pending: {
                    let s_name = "wl_display_dispatch_queue_pending\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *mut wl_event_queue,
                                ) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_create: {
                    let s_name = "wl_proxy_create\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    *const wl_interface,
                                ) -> *mut wl_proxy,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_destroy: {
                    let s_name = "wl_proxy_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_add_listener: {
                    let s_name = "wl_proxy_add_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    *mut extern "C" fn(),
                                    *mut c_void,
                                ) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_get_listener: {
                    let s_name = "wl_proxy_get_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy) -> *const c_void,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_add_dispatcher: {
                    let s_name = "wl_proxy_add_dispatcher\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    wl_dispatcher_func_t,
                                    *const c_void,
                                    *mut c_void,
                                ) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_marshal_array_constructor: {
                    let s_name = "wl_proxy_marshal_array_constructor\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    u32,
                                    *mut wl_argument,
                                    *const wl_interface,
                                ) -> *mut wl_proxy,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_marshal_array_constructor_versioned: {
                    let s_name = "wl_proxy_marshal_array_constructor_versioned\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    u32,
                                    *mut wl_argument,
                                    *const wl_interface,
                                    u32,
                                ) -> *mut wl_proxy,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_marshal_array: {
                    let s_name = "wl_proxy_marshal_array\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    u32,
                                    *mut wl_argument,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_set_user_data: {
                    let s_name = "wl_proxy_set_user_data\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy, *mut c_void) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_get_user_data: {
                    let s_name = "wl_proxy_get_user_data\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy) -> *mut c_void,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_get_id: {
                    let s_name = "wl_proxy_get_id\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy) -> u32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_get_class: {
                    let s_name = "wl_proxy_get_class\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy) -> *const c_char,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_set_queue: {
                    let s_name = "wl_proxy_set_queue\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    *mut wl_event_queue,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_get_version: {
                    let s_name = "wl_proxy_get_version\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy) -> u32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_create_wrapper: {
                    let s_name = "wl_proxy_create_wrapper\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy) -> *mut wl_proxy,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_wrapper_destroy: {
                    let s_name = "wl_proxy_wrapper_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_log_set_handler_client: {
                    let s_name = "wl_log_set_handler_client\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(wl_log_func_t) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_init: {
                    let s_name = "wl_list_init\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_insert: {
                    let s_name = "wl_list_insert\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list, *mut wl_list) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_remove: {
                    let s_name = "wl_list_remove\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_length: {
                    let s_name = "wl_list_length\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*const wl_list) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_empty: {
                    let s_name = "wl_list_empty\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*const wl_list) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_insert_list: {
                    let s_name = "wl_list_insert_list\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list, *mut wl_list) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_array_init: {
                    let s_name = "wl_array_init\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_array) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_array_release: {
                    let s_name = "wl_array_release\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_array) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_array_add: {
                    let s_name = "wl_array_add\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_array, usize) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_array_copy: {
                    let s_name = "wl_array_copy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_array, *mut wl_array) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_marshal_constructor: {
                    let s_name = "wl_proxy_marshal_constructor\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    u32,
                                    *const wl_interface,
                                    ...
                                ) -> *mut wl_proxy,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_marshal_constructor_versioned: {
                    let s_name = "wl_proxy_marshal_constructor_versioned\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    u32,
                                    *const wl_interface,
                                    u32,
                                    ...
                                ) -> *mut wl_proxy,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_proxy_marshal: {
                    let s_name = "wl_proxy_marshal\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_proxy, u32, ...) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                __lib: lib,
            };
            Ok(s)
        }
    }
    unsafe impl Sync for WaylandClient {}
    pub fn wayland_client_option() -> Option<&'static WaylandClient> {
        static WAYLAND_CLIENT_OPTION: Lazy<Option<WaylandClient>> = Lazy::new(|| {
            let versions = ["libwayland-client.so.0", "libwayland-client.so"];
            for ver in &versions {
                match unsafe { WaylandClient::open(ver) } {
                    Ok(h) => return Some(h),
                    Err(::dlib::DlError::CantOpen(_)) => continue,
                    Err(::dlib::DlError::MissingSymbol(s)) => {
                        {
                            {
                                let lvl = ::log::Level::Error;
                                if lvl <= ::log::STATIC_MAX_LEVEL
                                    && lvl <= ::log::max_level()
                                {
                                    ::log::__private_api::log(
                                        { ::log::__private_api::GlobalLogger },
                                        format_args!(
                                            "Found library {0} cannot be used: symbol {1} is missing.",
                                            ver,
                                            s,
                                        ),
                                        lvl,
                                        &(
                                            "wayland_sys::client",
                                            "wayland_sys::client",
                                            ::log::__private_api::loc(),
                                        ),
                                        (),
                                    );
                                }
                            }
                        };
                        return None;
                    }
                }
            }
            None
        });
        WAYLAND_CLIENT_OPTION.as_ref()
    }
    pub fn wayland_client_handle() -> &'static WaylandClient {
        static WAYLAND_CLIENT_HANDLE: Lazy<&'static WaylandClient> = Lazy::new(|| {
            wayland_client_option()
                .expect("Library libwayland-client.so could not be loaded.")
        });
        &WAYLAND_CLIENT_HANDLE
    }
    pub fn is_lib_available() -> bool {
        wayland_client_option().is_some()
    }
}
pub mod server {
    //! Bindings to the client library `libwayland-server.so`
    //!
    //! The generated handle is named `wayland_server_handle()`
    use super::common::*;
    use libc::{gid_t, pid_t, uid_t};
    use std::os::raw::c_char;
    use std::os::raw::{c_int, c_void};
    use once_cell::sync::Lazy;
    pub enum wl_client {}
    pub enum wl_display {}
    pub enum wl_event_loop {}
    pub enum wl_event_source {}
    pub enum wl_global {}
    pub enum wl_resource {}
    pub enum wl_shm_buffer {}
    pub type wl_event_loop_fd_func_t = unsafe extern "C" fn(
        c_int,
        u32,
        *mut c_void,
    ) -> c_int;
    pub type wl_event_loop_timer_func_t = unsafe extern "C" fn(*mut c_void) -> c_int;
    pub type wl_event_loop_signal_func_t = unsafe extern "C" fn(
        c_int,
        *mut c_void,
    ) -> c_int;
    pub type wl_event_loop_idle_func_t = unsafe extern "C" fn(*mut c_void) -> ();
    pub type wl_global_bind_func_t = unsafe extern "C" fn(
        *mut wl_client,
        *mut c_void,
        u32,
        u32,
    ) -> ();
    pub type wl_notify_func_t = unsafe extern "C" fn(
        *mut wl_listener,
        *mut c_void,
    ) -> ();
    pub type wl_resource_destroy_func_t = unsafe extern "C" fn(*mut wl_resource) -> ();
    pub type wl_display_global_filter_func_t = unsafe extern "C" fn(
        *const wl_client,
        *const wl_global,
        *mut c_void,
    ) -> bool;
    pub type wl_client_for_each_resource_iterator_func_t = unsafe extern "C" fn(
        *mut wl_resource,
        *mut c_void,
    ) -> c_int;
    #[repr(C)]
    pub struct wl_listener {
        pub link: wl_list,
        pub notify: wl_notify_func_t,
    }
    #[repr(C)]
    pub struct wl_signal {
        pub listener_list: wl_list,
    }
    pub struct WaylandServer {
        __lib: ::dlib::Library,
        pub wl_client_flush: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_client) -> (),
        >,
        pub wl_client_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_client) -> (),
        >,
        pub wl_client_get_display: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_client) -> *mut wl_display,
        >,
        pub wl_client_get_credentials: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_client,
                *mut pid_t,
                *mut uid_t,
                *mut gid_t,
            ) -> (),
        >,
        pub wl_client_get_object: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_client, u32) -> *mut wl_resource,
        >,
        pub wl_client_add_destroy_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_client, *mut wl_listener) -> (),
        >,
        pub wl_client_get_destroy_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_client, wl_notify_func_t) -> *mut wl_listener,
        >,
        pub wl_client_post_no_memory: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_client) -> (),
        >,
        pub wl_resource_create: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_client,
                *const wl_interface,
                c_int,
                u32,
            ) -> *mut wl_resource,
        >,
        pub wl_client_get_link: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_client) -> *mut wl_list,
        >,
        pub wl_client_from_link: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list) -> *mut wl_client,
        >,
        pub wl_client_for_each_resource: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_client,
                wl_client_for_each_resource_iterator_func_t,
                *mut c_void,
            ) -> (),
        >,
        pub wl_client_create: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, c_int) -> *mut wl_client,
        >,
        pub wl_display_create: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn() -> *mut wl_display,
        >,
        pub wl_display_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> (),
        >,
        pub wl_display_destroy_clients: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> (),
        >,
        pub wl_display_get_serial: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> u32,
        >,
        pub wl_display_next_serial: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> u32,
        >,
        pub wl_display_add_socket: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, *const c_char) -> c_int,
        >,
        pub wl_display_add_socket_auto: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> *const c_char,
        >,
        pub wl_display_add_socket_fd: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, c_int) -> c_int,
        >,
        pub wl_display_add_shm_format: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, u32) -> *mut u32,
        >,
        pub wl_display_get_event_loop: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> *mut wl_event_loop,
        >,
        pub wl_display_terminate: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> (),
        >,
        pub wl_display_run: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> (),
        >,
        pub wl_display_flush_clients: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> (),
        >,
        pub wl_display_add_destroy_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, *mut wl_listener) -> (),
        >,
        pub wl_display_get_destroy_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, wl_notify_func_t) -> *mut wl_listener,
        >,
        pub wl_global_create: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_display,
                *const wl_interface,
                c_int,
                *mut c_void,
                wl_global_bind_func_t,
            ) -> *mut wl_global,
        >,
        pub wl_display_init_shm: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> c_int,
        >,
        pub wl_display_add_client_created_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display, *mut wl_listener) -> (),
        >,
        pub wl_display_set_global_filter: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_display,
                wl_display_global_filter_func_t,
                *mut c_void,
            ) -> (),
        >,
        pub wl_display_get_client_list: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_display) -> *mut wl_list,
        >,
        pub wl_event_loop_create: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn() -> *mut wl_event_loop,
        >,
        pub wl_event_loop_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_loop) -> (),
        >,
        pub wl_event_loop_add_fd: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_event_loop,
                c_int,
                u32,
                wl_event_loop_fd_func_t,
                *mut c_void,
            ) -> *mut wl_event_source,
        >,
        pub wl_event_loop_add_timer: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_event_loop,
                wl_event_loop_timer_func_t,
                *mut c_void,
            ) -> *mut wl_event_source,
        >,
        pub wl_event_loop_add_signal: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_event_loop,
                c_int,
                wl_event_loop_signal_func_t,
                *mut c_void,
            ) -> *mut wl_event_source,
        >,
        pub wl_event_loop_dispatch: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_loop, c_int) -> c_int,
        >,
        pub wl_event_loop_dispatch_idle: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_loop) -> (),
        >,
        pub wl_event_loop_add_idle: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_event_loop,
                wl_event_loop_idle_func_t,
                *mut c_void,
            ) -> *mut wl_event_source,
        >,
        pub wl_event_loop_get_fd: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_loop) -> c_int,
        >,
        pub wl_event_loop_add_destroy_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_loop, *mut wl_listener) -> (),
        >,
        pub wl_event_loop_get_destroy_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_event_loop,
                wl_notify_func_t,
            ) -> *mut wl_listener,
        >,
        pub wl_event_source_fd_update: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_source, u32) -> c_int,
        >,
        pub wl_event_source_timer_update: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_source, c_int) -> c_int,
        >,
        pub wl_event_source_remove: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_source) -> c_int,
        >,
        pub wl_event_source_check: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_event_source) -> (),
        >,
        pub wl_global_remove: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_global) -> (),
        >,
        pub wl_global_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_global) -> (),
        >,
        pub wl_global_get_user_data: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*const wl_global) -> *mut c_void,
        >,
        pub wl_resource_post_event_array: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource, u32, *mut wl_argument) -> (),
        >,
        pub wl_resource_queue_event_array: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource, u32, *mut wl_argument) -> (),
        >,
        pub wl_resource_post_no_memory: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> (),
        >,
        pub wl_resource_set_implementation: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_resource,
                *const c_void,
                *mut c_void,
                Option<wl_resource_destroy_func_t>,
            ) -> (),
        >,
        pub wl_resource_set_dispatcher: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_resource,
                wl_dispatcher_func_t,
                *const c_void,
                *mut c_void,
                Option<wl_resource_destroy_func_t>,
            ) -> (),
        >,
        pub wl_resource_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> (),
        >,
        pub wl_resource_get_client: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> *mut wl_client,
        >,
        pub wl_resource_get_id: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> u32,
        >,
        pub wl_resource_get_link: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> *mut wl_list,
        >,
        pub wl_resource_from_link: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list) -> *mut wl_resource,
        >,
        pub wl_resource_find_for_client: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list, *mut wl_client) -> (),
        >,
        pub wl_resource_set_user_data: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource, *mut c_void) -> (),
        >,
        pub wl_resource_get_user_data: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> *mut c_void,
        >,
        pub wl_resource_get_version: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> c_int,
        >,
        pub wl_resource_get_class: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> *const c_char,
        >,
        pub wl_resource_set_destructor: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_resource,
                Option<wl_resource_destroy_func_t>,
            ) -> (),
        >,
        pub wl_resource_instance_of: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *mut wl_resource,
                *const wl_interface,
                *const c_void,
            ) -> c_int,
        >,
        pub wl_resource_add_destroy_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource, *mut wl_listener) -> (),
        >,
        pub wl_resource_get_destroy_listener: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource, wl_notify_func_t) -> *mut wl_listener,
        >,
        pub wl_shm_buffer_begin_access: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_shm_buffer) -> (),
        >,
        pub wl_shm_buffer_end_access: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_shm_buffer) -> (),
        >,
        pub wl_shm_buffer_get: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource) -> *mut wl_shm_buffer,
        >,
        pub wl_shm_buffer_get_data: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_shm_buffer) -> *mut c_void,
        >,
        pub wl_shm_buffer_get_stride: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_shm_buffer) -> i32,
        >,
        pub wl_shm_buffer_get_format: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_shm_buffer) -> u32,
        >,
        pub wl_shm_buffer_get_width: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_shm_buffer) -> i32,
        >,
        pub wl_shm_buffer_get_height: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_shm_buffer) -> i32,
        >,
        pub wl_log_set_handler_server: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(wl_log_func_t) -> (),
        >,
        pub wl_list_init: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list) -> (),
        >,
        pub wl_list_insert: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list, *mut wl_list) -> (),
        >,
        pub wl_list_remove: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list) -> (),
        >,
        pub wl_list_length: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*const wl_list) -> c_int,
        >,
        pub wl_list_empty: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*const wl_list) -> c_int,
        >,
        pub wl_list_insert_list: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_list, *mut wl_list) -> (),
        >,
        pub wl_array_init: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_array) -> (),
        >,
        pub wl_array_release: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_array) -> (),
        >,
        pub wl_array_add: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_array, usize) -> (),
        >,
        pub wl_array_copy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_array, *mut wl_array) -> (),
        >,
        pub wl_resource_post_event: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource, u32, ...) -> (),
        >,
        pub wl_resource_queue_event: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource, u32, ...) -> (),
        >,
        pub wl_resource_post_error: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_resource, u32, *const c_char, ...) -> (),
        >,
    }
    impl WaylandServer {
        pub unsafe fn open(name: &str) -> Result<WaylandServer, ::dlib::DlError> {
            use std::mem::transmute;
            let lib = ::dlib::Library::new(name).map_err(::dlib::DlError::CantOpen)?;
            let s = WaylandServer {
                wl_client_flush: {
                    let s_name = "wl_client_flush\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_client) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_destroy: {
                    let s_name = "wl_client_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_client) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_get_display: {
                    let s_name = "wl_client_get_display\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_client) -> *mut wl_display,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_get_credentials: {
                    let s_name = "wl_client_get_credentials\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_client,
                                    *mut pid_t,
                                    *mut uid_t,
                                    *mut gid_t,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_get_object: {
                    let s_name = "wl_client_get_object\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_client,
                                    u32,
                                ) -> *mut wl_resource,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_add_destroy_listener: {
                    let s_name = "wl_client_add_destroy_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_client, *mut wl_listener) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_get_destroy_listener: {
                    let s_name = "wl_client_get_destroy_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_client,
                                    wl_notify_func_t,
                                ) -> *mut wl_listener,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_post_no_memory: {
                    let s_name = "wl_client_post_no_memory\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_client) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_create: {
                    let s_name = "wl_resource_create\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_client,
                                    *const wl_interface,
                                    c_int,
                                    u32,
                                ) -> *mut wl_resource,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_get_link: {
                    let s_name = "wl_client_get_link\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_client) -> *mut wl_list,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_from_link: {
                    let s_name = "wl_client_from_link\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list) -> *mut wl_client,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_for_each_resource: {
                    let s_name = "wl_client_for_each_resource\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_client,
                                    wl_client_for_each_resource_iterator_func_t,
                                    *mut c_void,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_client_create: {
                    let s_name = "wl_client_create\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    c_int,
                                ) -> *mut wl_client,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_create: {
                    let s_name = "wl_display_create\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn() -> *mut wl_display,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_destroy: {
                    let s_name = "wl_display_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_destroy_clients: {
                    let s_name = "wl_display_destroy_clients\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_get_serial: {
                    let s_name = "wl_display_get_serial\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> u32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_next_serial: {
                    let s_name = "wl_display_next_serial\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> u32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_add_socket: {
                    let s_name = "wl_display_add_socket\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *const c_char,
                                ) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_add_socket_auto: {
                    let s_name = "wl_display_add_socket_auto\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> *const c_char,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_add_socket_fd: {
                    let s_name = "wl_display_add_socket_fd\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display, c_int) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_add_shm_format: {
                    let s_name = "wl_display_add_shm_format\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display, u32) -> *mut u32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_get_event_loop: {
                    let s_name = "wl_display_get_event_loop\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> *mut wl_event_loop,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_terminate: {
                    let s_name = "wl_display_terminate\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_run: {
                    let s_name = "wl_display_run\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_flush_clients: {
                    let s_name = "wl_display_flush_clients\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_add_destroy_listener: {
                    let s_name = "wl_display_add_destroy_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *mut wl_listener,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_get_destroy_listener: {
                    let s_name = "wl_display_get_destroy_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    wl_notify_func_t,
                                ) -> *mut wl_listener,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_global_create: {
                    let s_name = "wl_global_create\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *const wl_interface,
                                    c_int,
                                    *mut c_void,
                                    wl_global_bind_func_t,
                                ) -> *mut wl_global,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_init_shm: {
                    let s_name = "wl_display_init_shm\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_add_client_created_listener: {
                    let s_name = "wl_display_add_client_created_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    *mut wl_listener,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_set_global_filter: {
                    let s_name = "wl_display_set_global_filter\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_display,
                                    wl_display_global_filter_func_t,
                                    *mut c_void,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_display_get_client_list: {
                    let s_name = "wl_display_get_client_list\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_display) -> *mut wl_list,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_create: {
                    let s_name = "wl_event_loop_create\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn() -> *mut wl_event_loop,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_destroy: {
                    let s_name = "wl_event_loop_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_loop) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_add_fd: {
                    let s_name = "wl_event_loop_add_fd\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_event_loop,
                                    c_int,
                                    u32,
                                    wl_event_loop_fd_func_t,
                                    *mut c_void,
                                ) -> *mut wl_event_source,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_add_timer: {
                    let s_name = "wl_event_loop_add_timer\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_event_loop,
                                    wl_event_loop_timer_func_t,
                                    *mut c_void,
                                ) -> *mut wl_event_source,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_add_signal: {
                    let s_name = "wl_event_loop_add_signal\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_event_loop,
                                    c_int,
                                    wl_event_loop_signal_func_t,
                                    *mut c_void,
                                ) -> *mut wl_event_source,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_dispatch: {
                    let s_name = "wl_event_loop_dispatch\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_loop, c_int) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_dispatch_idle: {
                    let s_name = "wl_event_loop_dispatch_idle\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_loop) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_add_idle: {
                    let s_name = "wl_event_loop_add_idle\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_event_loop,
                                    wl_event_loop_idle_func_t,
                                    *mut c_void,
                                ) -> *mut wl_event_source,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_get_fd: {
                    let s_name = "wl_event_loop_get_fd\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_loop) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_add_destroy_listener: {
                    let s_name = "wl_event_loop_add_destroy_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_event_loop,
                                    *mut wl_listener,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_loop_get_destroy_listener: {
                    let s_name = "wl_event_loop_get_destroy_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_event_loop,
                                    wl_notify_func_t,
                                ) -> *mut wl_listener,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_source_fd_update: {
                    let s_name = "wl_event_source_fd_update\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_source, u32) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_source_timer_update: {
                    let s_name = "wl_event_source_timer_update\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_source, c_int) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_source_remove: {
                    let s_name = "wl_event_source_remove\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_source) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_event_source_check: {
                    let s_name = "wl_event_source_check\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_event_source) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_global_remove: {
                    let s_name = "wl_global_remove\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_global) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_global_destroy: {
                    let s_name = "wl_global_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_global) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_global_get_user_data: {
                    let s_name = "wl_global_get_user_data\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*const wl_global) -> *mut c_void,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_post_event_array: {
                    let s_name = "wl_resource_post_event_array\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    u32,
                                    *mut wl_argument,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_queue_event_array: {
                    let s_name = "wl_resource_queue_event_array\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    u32,
                                    *mut wl_argument,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_post_no_memory: {
                    let s_name = "wl_resource_post_no_memory\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_set_implementation: {
                    let s_name = "wl_resource_set_implementation\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    *const c_void,
                                    *mut c_void,
                                    Option<wl_resource_destroy_func_t>,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_set_dispatcher: {
                    let s_name = "wl_resource_set_dispatcher\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    wl_dispatcher_func_t,
                                    *const c_void,
                                    *mut c_void,
                                    Option<wl_resource_destroy_func_t>,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_destroy: {
                    let s_name = "wl_resource_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_get_client: {
                    let s_name = "wl_resource_get_client\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> *mut wl_client,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_get_id: {
                    let s_name = "wl_resource_get_id\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> u32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_get_link: {
                    let s_name = "wl_resource_get_link\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> *mut wl_list,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_from_link: {
                    let s_name = "wl_resource_from_link\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list) -> *mut wl_resource,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_find_for_client: {
                    let s_name = "wl_resource_find_for_client\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list, *mut wl_client) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_set_user_data: {
                    let s_name = "wl_resource_set_user_data\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource, *mut c_void) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_get_user_data: {
                    let s_name = "wl_resource_get_user_data\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> *mut c_void,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_get_version: {
                    let s_name = "wl_resource_get_version\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_get_class: {
                    let s_name = "wl_resource_get_class\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> *const c_char,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_set_destructor: {
                    let s_name = "wl_resource_set_destructor\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    Option<wl_resource_destroy_func_t>,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_instance_of: {
                    let s_name = "wl_resource_instance_of\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    *const wl_interface,
                                    *const c_void,
                                ) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_add_destroy_listener: {
                    let s_name = "wl_resource_add_destroy_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    *mut wl_listener,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_get_destroy_listener: {
                    let s_name = "wl_resource_get_destroy_listener\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    wl_notify_func_t,
                                ) -> *mut wl_listener,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_shm_buffer_begin_access: {
                    let s_name = "wl_shm_buffer_begin_access\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_shm_buffer) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_shm_buffer_end_access: {
                    let s_name = "wl_shm_buffer_end_access\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_shm_buffer) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_shm_buffer_get: {
                    let s_name = "wl_shm_buffer_get\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource) -> *mut wl_shm_buffer,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_shm_buffer_get_data: {
                    let s_name = "wl_shm_buffer_get_data\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_shm_buffer) -> *mut c_void,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_shm_buffer_get_stride: {
                    let s_name = "wl_shm_buffer_get_stride\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_shm_buffer) -> i32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_shm_buffer_get_format: {
                    let s_name = "wl_shm_buffer_get_format\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_shm_buffer) -> u32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_shm_buffer_get_width: {
                    let s_name = "wl_shm_buffer_get_width\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_shm_buffer) -> i32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_shm_buffer_get_height: {
                    let s_name = "wl_shm_buffer_get_height\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_shm_buffer) -> i32,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_log_set_handler_server: {
                    let s_name = "wl_log_set_handler_server\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(wl_log_func_t) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_init: {
                    let s_name = "wl_list_init\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_insert: {
                    let s_name = "wl_list_insert\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list, *mut wl_list) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_remove: {
                    let s_name = "wl_list_remove\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_length: {
                    let s_name = "wl_list_length\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*const wl_list) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_empty: {
                    let s_name = "wl_list_empty\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*const wl_list) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_list_insert_list: {
                    let s_name = "wl_list_insert_list\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_list, *mut wl_list) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_array_init: {
                    let s_name = "wl_array_init\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_array) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_array_release: {
                    let s_name = "wl_array_release\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_array) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_array_add: {
                    let s_name = "wl_array_add\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_array, usize) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_array_copy: {
                    let s_name = "wl_array_copy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_array, *mut wl_array) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_post_event: {
                    let s_name = "wl_resource_post_event\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource, u32, ...) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_queue_event: {
                    let s_name = "wl_resource_queue_event\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_resource, u32, ...) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_resource_post_error: {
                    let s_name = "wl_resource_post_error\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_resource,
                                    u32,
                                    *const c_char,
                                    ...
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                __lib: lib,
            };
            Ok(s)
        }
    }
    unsafe impl Sync for WaylandServer {}
    pub fn wayland_server_option() -> Option<&'static WaylandServer> {
        static WAYLAND_SERVER_OPTION: Lazy<Option<WaylandServer>> = Lazy::new(|| {
            let versions = ["libwayland-server.so.0", "libwayland-server.so"];
            for ver in &versions {
                match unsafe { WaylandServer::open(ver) } {
                    Ok(h) => return Some(h),
                    Err(::dlib::DlError::CantOpen(_)) => continue,
                    Err(::dlib::DlError::MissingSymbol(s)) => {
                        {
                            {
                                let lvl = ::log::Level::Error;
                                if lvl <= ::log::STATIC_MAX_LEVEL
                                    && lvl <= ::log::max_level()
                                {
                                    ::log::__private_api::log(
                                        { ::log::__private_api::GlobalLogger },
                                        format_args!(
                                            "Found library {0} cannot be used: symbol {1} is missing.",
                                            ver,
                                            s,
                                        ),
                                        lvl,
                                        &(
                                            "wayland_sys::server",
                                            "wayland_sys::server",
                                            ::log::__private_api::loc(),
                                        ),
                                        (),
                                    );
                                }
                            }
                        };
                        return None;
                    }
                }
            }
            None
        });
        WAYLAND_SERVER_OPTION.as_ref()
    }
    pub fn wayland_server_handle() -> &'static WaylandServer {
        static WAYLAND_SERVER_HANDLE: Lazy<&'static WaylandServer> = Lazy::new(|| {
            wayland_server_option()
                .expect("Library libwayland-server.so could not be loaded.")
        });
        &WAYLAND_SERVER_HANDLE
    }
    pub fn is_lib_available() -> bool {
        wayland_server_option().is_some()
    }
    pub mod signal {
        #![allow(clippy::cast_ptr_alignment, clippy::missing_safety_doc)]
        use super::wayland_server_handle as wsh;
        use super::{wl_listener, wl_notify_func_t, wl_signal};
        use crate::common::wl_list;
        use std::os::raw::c_void;
        use std::ptr;
        pub unsafe fn wl_signal_init(signal: *mut wl_signal) {
            {
                let ret = (wsh().wl_list_init)(unsafe { &mut (*signal).listener_list });
                ret
            };
        }
        pub unsafe fn wl_signal_add(signal: *mut wl_signal, listener: *mut wl_listener) {
            {
                let ret = (wsh()
                    .wl_list_insert)(
                    unsafe { (*signal).listener_list.prev },
                    unsafe { &mut (*listener).link },
                );
                ret
            }
        }
        pub unsafe fn wl_signal_get(
            signal: *mut wl_signal,
            notify: wl_notify_func_t,
        ) -> *mut wl_listener {
            unsafe {
                let mut l = ((*(&mut (*signal).listener_list as *mut wl_list)).next
                    as *mut u8)
                    .sub({ const { builtin # offset_of(wl_listener, link) } })
                    as *mut wl_listener;
                while &mut (*l).link as *mut _
                    != &mut (*signal).listener_list as *mut wl_list
                {
                    {
                        #[allow(unknown_lints)]
                        #[allow(unpredictable_function_pointer_comparisons)]
                        if (*l).notify == notify {
                            return l;
                        }
                    };
                    l = ((*l).link.next as *mut u8)
                        .sub({ const { builtin # offset_of(wl_listener, link) } })
                        as *mut wl_listener;
                }
            }
            ptr::null_mut()
        }
        pub unsafe fn wl_signal_emit(signal: *mut wl_signal, data: *mut c_void) {
            unsafe {
                let mut l = ((*(&mut (*signal).listener_list as *mut wl_list)).next
                    as *mut u8)
                    .sub({ const { builtin # offset_of(wl_listener, link) } })
                    as *mut wl_listener;
                let mut tmp = ((*l).link.next as *mut u8)
                    .sub({ const { builtin # offset_of(wl_listener, link) } })
                    as *mut wl_listener;
                while &mut (*l).link as *mut _
                    != &mut (*signal).listener_list as *mut wl_list
                {
                    {
                        ((*l).notify)(l, data);
                    };
                    l = tmp;
                    tmp = ((*l).link.next as *mut u8)
                        .sub({ const { builtin # offset_of(wl_listener, link) } })
                        as *mut wl_listener;
                }
            }
        }
        #[repr(C)]
        struct ListenerWithUserData {
            listener: wl_listener,
            user_data: *mut c_void,
        }
        pub fn rust_listener_create(notify: wl_notify_func_t) -> *mut wl_listener {
            let data = Box::into_raw(
                Box::new(ListenerWithUserData {
                    listener: wl_listener {
                        link: wl_list {
                            prev: ptr::null_mut(),
                            next: ptr::null_mut(),
                        },
                        notify,
                    },
                    user_data: ptr::null_mut(),
                }),
            );
            unsafe { &raw mut (*data).listener }
        }
        pub unsafe fn rust_listener_get_user_data(
            listener: *mut wl_listener,
        ) -> *mut c_void {
            let data = unsafe {
                (listener as *mut u8)
                    .sub({
                        const { builtin # offset_of(ListenerWithUserData, listener) }
                    }) as *mut ListenerWithUserData
            };
            unsafe { (*data).user_data }
        }
        pub unsafe fn rust_listener_set_user_data(
            listener: *mut wl_listener,
            user_data: *mut c_void,
        ) {
            let data = unsafe {
                (listener as *mut u8)
                    .sub({
                        const { builtin # offset_of(ListenerWithUserData, listener) }
                    }) as *mut ListenerWithUserData
            };
            unsafe { (*data).user_data = user_data }
        }
        pub unsafe fn rust_listener_destroy(listener: *mut wl_listener) {
            let data = unsafe {
                (listener as *mut u8)
                    .sub({
                        const { builtin # offset_of(ListenerWithUserData, listener) }
                    }) as *mut ListenerWithUserData
            };
            let _ = unsafe { Box::from_raw(data) };
        }
    }
}
pub mod egl {
    //! Bindings to the EGL library `libwayland-egl.so`
    //!
    //! This lib allows to create EGL surfaces out of wayland surfaces.
    //!
    //! The created handle is named `wayland_egl_handle()`.
    use crate::client::wl_proxy;
    use once_cell::sync::Lazy;
    use std::os::raw::c_int;
    pub enum wl_egl_window {}
    pub struct WaylandEgl {
        __lib: ::dlib::Library,
        pub wl_egl_window_create: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_proxy, c_int, c_int) -> *mut wl_egl_window,
        >,
        pub wl_egl_window_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_egl_window) -> (),
        >,
        pub wl_egl_window_resize: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_egl_window, c_int, c_int, c_int, c_int) -> (),
        >,
        pub wl_egl_window_get_attached_size: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_egl_window, *mut c_int, *mut c_int) -> (),
        >,
    }
    impl WaylandEgl {
        pub unsafe fn open(name: &str) -> Result<WaylandEgl, ::dlib::DlError> {
            use std::mem::transmute;
            let lib = ::dlib::Library::new(name).map_err(::dlib::DlError::CantOpen)?;
            let s = WaylandEgl {
                wl_egl_window_create: {
                    let s_name = "wl_egl_window_create\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_proxy,
                                    c_int,
                                    c_int,
                                ) -> *mut wl_egl_window,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_egl_window_destroy: {
                    let s_name = "wl_egl_window_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_egl_window) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_egl_window_resize: {
                    let s_name = "wl_egl_window_resize\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_egl_window,
                                    c_int,
                                    c_int,
                                    c_int,
                                    c_int,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_egl_window_get_attached_size: {
                    let s_name = "wl_egl_window_get_attached_size\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_egl_window,
                                    *mut c_int,
                                    *mut c_int,
                                ) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                __lib: lib,
            };
            Ok(s)
        }
    }
    unsafe impl Sync for WaylandEgl {}
    pub fn wayland_egl_option() -> Option<&'static WaylandEgl> {
        static WAYLAND_EGL_OPTION: Lazy<Option<WaylandEgl>> = Lazy::new(|| {
            let versions = ["libwayland-egl.so.1", "libwayland-egl.so"];
            for ver in &versions {
                match unsafe { WaylandEgl::open(ver) } {
                    Ok(h) => return Some(h),
                    Err(::dlib::DlError::CantOpen(_)) => continue,
                    Err(::dlib::DlError::MissingSymbol(s)) => {
                        {
                            {
                                let lvl = ::log::Level::Error;
                                if lvl <= ::log::STATIC_MAX_LEVEL
                                    && lvl <= ::log::max_level()
                                {
                                    ::log::__private_api::log(
                                        { ::log::__private_api::GlobalLogger },
                                        format_args!(
                                            "Found library {0} cannot be used: symbol {1} is missing.",
                                            ver,
                                            s,
                                        ),
                                        lvl,
                                        &(
                                            "wayland_sys::egl",
                                            "wayland_sys::egl",
                                            ::log::__private_api::loc(),
                                        ),
                                        (),
                                    );
                                }
                            }
                        };
                        return None;
                    }
                }
            }
            None
        });
        WAYLAND_EGL_OPTION.as_ref()
    }
    pub fn wayland_egl_handle() -> &'static WaylandEgl {
        static WAYLAND_EGL_HANDLE: Lazy<&'static WaylandEgl> = Lazy::new(|| {
            wayland_egl_option().expect("Library libwayland-egl.so could not be loaded.")
        });
        &WAYLAND_EGL_HANDLE
    }
    pub fn is_lib_available() -> bool {
        wayland_egl_option().is_some()
    }
}
pub mod cursor {
    //! Bindings to the `wayland-cursor.so` library
    //!
    //! The created handle is named `wayland_cursor_handle()`.
    use crate::client::wl_proxy;
    use once_cell::sync::Lazy;
    use std::os::raw::{c_char, c_int, c_uint};
    pub enum wl_cursor_theme {}
    #[repr(C)]
    pub struct wl_cursor_image {
        /// actual width
        pub width: u32,
        /// actual height
        pub height: u32,
        /// hot spot x (must be inside image)
        pub hotspot_x: u32,
        /// hot spot y (must be inside image)
        pub hotspot_y: u32,
        /// animation delay to next frame
        pub delay: u32,
    }
    #[repr(C)]
    pub struct wl_cursor {
        pub image_count: c_uint,
        pub images: *mut *mut wl_cursor_image,
        pub name: *mut c_char,
    }
    pub struct WaylandCursor {
        __lib: ::dlib::Library,
        pub wl_cursor_theme_load: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(
                *const c_char,
                c_int,
                *mut wl_proxy,
            ) -> *mut wl_cursor_theme,
        >,
        pub wl_cursor_theme_destroy: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_cursor_theme) -> (),
        >,
        pub wl_cursor_theme_get_cursor: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_cursor_theme, *const c_char) -> *mut wl_cursor,
        >,
        pub wl_cursor_image_get_buffer: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_cursor_image) -> *mut wl_proxy,
        >,
        pub wl_cursor_frame: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_cursor, u32) -> c_int,
        >,
        pub wl_cursor_frame_and_duration: ::dlib::Symbol<
            'static,
            unsafe extern "C" fn(*mut wl_cursor, u32, *mut u32) -> c_int,
        >,
    }
    impl WaylandCursor {
        pub unsafe fn open(name: &str) -> Result<WaylandCursor, ::dlib::DlError> {
            use std::mem::transmute;
            let lib = ::dlib::Library::new(name).map_err(::dlib::DlError::CantOpen)?;
            let s = WaylandCursor {
                wl_cursor_theme_load: {
                    let s_name = "wl_cursor_theme_load\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *const c_char,
                                    c_int,
                                    *mut wl_proxy,
                                ) -> *mut wl_cursor_theme,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_cursor_theme_destroy: {
                    let s_name = "wl_cursor_theme_destroy\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_cursor_theme) -> (),
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_cursor_theme_get_cursor: {
                    let s_name = "wl_cursor_theme_get_cursor\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(
                                    *mut wl_cursor_theme,
                                    *const c_char,
                                ) -> *mut wl_cursor,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_cursor_image_get_buffer: {
                    let s_name = "wl_cursor_image_get_buffer\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_cursor_image) -> *mut wl_proxy,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_cursor_frame: {
                    let s_name = "wl_cursor_frame\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_cursor, u32) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                wl_cursor_frame_and_duration: {
                    let s_name = "wl_cursor_frame_and_duration\u{0}";
                    transmute(
                        match lib
                            .get::<
                                unsafe extern "C" fn(*mut wl_cursor, u32, *mut u32) -> c_int,
                            >(s_name.as_bytes())
                        {
                            Ok(s) => s,
                            Err(_) => return Err(::dlib::DlError::MissingSymbol(s_name)),
                        },
                    )
                },
                __lib: lib,
            };
            Ok(s)
        }
    }
    unsafe impl Sync for WaylandCursor {}
    pub fn wayland_cursor_option() -> Option<&'static WaylandCursor> {
        static WAYLAND_CURSOR_OPTION: Lazy<Option<WaylandCursor>> = Lazy::new(|| {
            let versions = ["libwayland-cursor.so.0", "libwayland-cursor.so"];
            for ver in &versions {
                match unsafe { WaylandCursor::open(ver) } {
                    Ok(h) => return Some(h),
                    Err(::dlib::DlError::CantOpen(_)) => continue,
                    Err(::dlib::DlError::MissingSymbol(s)) => {
                        {
                            {
                                let lvl = ::log::Level::Error;
                                if lvl <= ::log::STATIC_MAX_LEVEL
                                    && lvl <= ::log::max_level()
                                {
                                    ::log::__private_api::log(
                                        { ::log::__private_api::GlobalLogger },
                                        format_args!(
                                            "Found library {0} cannot be used: symbol {1} is missing.",
                                            ver,
                                            s,
                                        ),
                                        lvl,
                                        &(
                                            "wayland_sys::cursor",
                                            "wayland_sys::cursor",
                                            ::log::__private_api::loc(),
                                        ),
                                        (),
                                    );
                                }
                            }
                        };
                        return None;
                    }
                }
            }
            None
        });
        WAYLAND_CURSOR_OPTION.as_ref()
    }
    pub fn wayland_cursor_handle() -> &'static WaylandCursor {
        static WAYLAND_CURSOR_HANDLE: Lazy<&'static WaylandCursor> = Lazy::new(|| {
            wayland_cursor_option()
                .expect("Library libwayland-cursor.so could not be loaded.")
        });
        &WAYLAND_CURSOR_HANDLE
    }
    pub fn is_lib_available() -> bool {
        wayland_cursor_option().is_some()
    }
}
pub use libc::{gid_t, pid_t, uid_t};
