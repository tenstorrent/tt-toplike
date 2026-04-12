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
    use std::os::raw::{c_int, c_void};
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
}
