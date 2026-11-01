// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::ffi::CString;

use crate::error::UbusError;
use crate::raw::{blob_attr, blob_buf, LibUbus};

pub struct BlobBuffer {
    lib: LibUbus,
    raw: blob_buf,
}

impl BlobBuffer {
    pub fn new(lib: LibUbus) -> Self {
        let mut raw = blob_buf {
            head: std::ptr::null_mut(),
            grow: None,
            buflen: 0,
            buf: std::ptr::null_mut(),
        };
        unsafe {
            lib.blob_buf_init(&mut raw, 0);
        }
        Self { lib, raw }
    }

    pub fn from_json_str(lib: LibUbus, json_str: &str) -> Result<Self, UbusError> {
        let mut buf = Self::new(lib);
        let c_str = CString::new(json_str)?;
        unsafe {
            buf.lib
                .blobmsg_add_json_from_string(&mut buf.raw, c_str.as_ptr());
        }
        Ok(buf)
    }

    #[inline]
    pub fn as_raw_ptr(&mut self) -> *mut blob_buf {
        &mut self.raw
    }

    #[inline]
    pub fn head(&mut self) -> *mut blob_attr {
        self.raw.head
    }
}

impl Drop for BlobBuffer {
    fn drop(&mut self) {
        unsafe {
            self.lib.blob_buf_free(&mut self.raw);
        }
    }
}
