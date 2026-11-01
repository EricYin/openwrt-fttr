// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

use serde_json::Value;

use crate::blobmsg::BlobBuffer;
use crate::error::UbusError;
use crate::raw::*;

pub type MethodHandler =
    Box<dyn Fn(&str, Option<&Value>) -> Result<Value, c_int> + Send + Sync + 'static>;

#[repr(C)]
struct RustUbusObject {
    c_obj: ubus_object,
    state_ptr: *mut ObjectState,
}

#[allow(dead_code)]
struct ObjectState {
    name: CString,
    type_name: CString,
    c_method_names: Vec<CString>,
    c_methods: Vec<ubus_method>,
    object_type: Box<ubus_object_type>,
    rust_obj: Box<RustUbusObject>,
    handlers: HashMap<String, MethodHandler>,
    lib: LibUbus,
}

unsafe extern "C" fn server_method_dispatch(
    ctx: *mut ubus_context,
    obj: *mut ubus_object,
    req: *mut ubus_request_data,
    method: *const c_char,
    msg: *mut blob_attr,
) -> c_int {
    if obj.is_null() || method.is_null() {
        return UBUS_STATUS_INVALID_ARGUMENT;
    }
    let method_name = match CStr::from_ptr(method).to_str() {
        Ok(s) => s,
        Err(_) => return UBUS_STATUS_INVALID_ARGUMENT,
    };

    let rust_obj = &*(obj as *const RustUbusObject);
    let state = &*rust_obj.state_ptr;

    let parsed_msg = if !msg.is_null() {
        state
            .lib
            .blobmsg_format_json(msg, true)
            .and_then(|j| serde_json::from_str::<Value>(&j).ok())
    } else {
        None
    };

    if let Some(handler) = state.handlers.get(method_name) {
        match handler(method_name, parsed_msg.as_ref()) {
            Ok(reply_val) => {
                if !reply_val.is_null() {
                    if let Ok(json_str) = serde_json::to_string(&reply_val) {
                        if let Ok(mut blob) =
                            BlobBuffer::from_json_str(state.lib.clone(), &json_str)
                        {
                            state.lib.ubus_send_reply(ctx, req, blob.head());
                        }
                    }
                }
                UBUS_STATUS_OK
            }
            Err(code) => code,
        }
    } else {
        UBUS_STATUS_METHOD_NOT_FOUND
    }
}

pub struct UbusServer {
    lib: LibUbus,
    ctx: *mut ubus_context,
    objects: Vec<Box<ObjectState>>,
}

unsafe impl Send for UbusServer {}

impl UbusServer {
    pub fn new(socket_path: Option<&str>) -> Result<Self, UbusError> {
        let lib = LibUbus::load().map_err(UbusError::LoadLibrary)?;
        Self::new_with_lib(lib, socket_path)
    }

    pub fn new_with_lib(lib: LibUbus, socket_path: Option<&str>) -> Result<Self, UbusError> {
        let c_path = match socket_path {
            Some(p) => Some(CString::new(p)?),
            None => None,
        };
        let path_ptr = c_path
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        unsafe {
            let ctx = lib.ubus_connect(path_ptr);
            if ctx.is_null() {
                return Err(UbusError::ConnectionFailed(
                    socket_path
                        .unwrap_or("default /var/run/ubus/ubus.sock")
                        .to_string(),
                ));
            }
            Ok(Self {
                lib,
                ctx,
                objects: Vec::new(),
            })
        }
    }

    pub fn add_object<I>(&mut self, object_name: &str, methods: I) -> Result<(), UbusError>
    where
        I: IntoIterator<Item = (String, MethodHandler)>,
    {
        let c_name = CString::new(object_name)?;
        let type_name = CString::new(format!("{object_name}_type"))?;

        let mut handlers = HashMap::new();
        let mut c_method_names = Vec::new();
        let mut c_methods = Vec::new();

        for (name, handler) in methods {
            let c_mname = CString::new(name.clone())?;
            c_methods.push(ubus_method {
                name: c_mname.as_ptr(),
                handler: Some(server_method_dispatch),
                mask: 0,
                tags: 0,
                policy: std::ptr::null(),
                n_policy: 0,
            });
            c_method_names.push(c_mname);
            handlers.insert(name, handler);
        }

        let mut object_type = Box::new(ubus_object_type {
            name: type_name.as_ptr(),
            id: 0,
            methods: c_methods.as_ptr(),
            n_methods: c_methods.len() as c_int,
        });

        let rust_obj = Box::new(RustUbusObject {
            c_obj: ubus_object {
                avl: avl_node {
                    list: list_head {
                        next: std::ptr::null_mut(),
                        prev: std::ptr::null_mut(),
                    },
                    parent: std::ptr::null_mut(),
                    left: std::ptr::null_mut(),
                    right: std::ptr::null_mut(),
                    key: std::ptr::null(),
                    balance: 0,
                    leader: false,
                },
                name: c_name.as_ptr(),
                id: 0,
                path: std::ptr::null(),
                type_: &mut *object_type,
                subscribe_cb: None,
                has_subscribers: false,
                methods: c_methods.as_ptr(),
                n_methods: c_methods.len() as c_int,
            },
            state_ptr: std::ptr::null_mut(),
        });

        let mut state = Box::new(ObjectState {
            name: c_name,
            type_name,
            c_method_names,
            c_methods,
            object_type,
            rust_obj,
            handlers,
            lib: self.lib.clone(),
        });

        // Set backlink pointer
        state.rust_obj.state_ptr = &mut *state as *mut ObjectState;

        unsafe {
            let ret = self
                .lib
                .ubus_add_object(self.ctx, &mut state.rust_obj.c_obj);
            if ret != UBUS_STATUS_OK {
                return Err(UbusError::InvokeFailed(format!(
                    "ubus_add_object({}) failed with: {}",
                    object_name,
                    self.lib.strerror(ret)
                )));
            }
        }

        self.objects.push(state);
        Ok(())
    }

    pub fn run_loop(&self) -> Result<(), UbusError> {
        unsafe {
            self.lib.uloop_init();
            let ret = self.lib.ubus_add_uloop(self.ctx);
            if ret != 0 {
                return Err(UbusError::Backend(format!(
                    "ubus_add_uloop failed with return code {ret}"
                )));
            }
            self.lib.uloop_run();
            self.lib.uloop_done();
            Ok(())
        }
    }

    pub fn stop_loop(&self) {
        unsafe {
            self.lib.uloop_end();
        }
    }
}

impl Drop for UbusServer {
    fn drop(&mut self) {
        unsafe {
            for obj in &mut self.objects {
                self.lib
                    .ubus_remove_object(self.ctx, &mut obj.rust_obj.c_obj);
            }
            if !self.ctx.is_null() {
                self.lib.ubus_free(self.ctx);
            }
        }
    }
}
