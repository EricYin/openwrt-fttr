// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
use std::str::FromStr;

use crate::error::UciError;
use crate::raw::*;

#[derive(Debug, Clone)]
pub struct UciSectionInfo {
    pub name: String,
    pub section_type: String,
    pub anonymous: bool,
}

pub struct Uci {
    lib: LibUci,
    ctx: *mut uci_context,
}

unsafe impl Send for Uci {}

impl Uci {
    pub fn new() -> Result<Self, UciError> {
        let lib = LibUci::load().map_err(UciError::LoadLibrary)?;
        Self::new_with_lib(lib)
    }

    pub fn new_with_lib(lib: LibUci) -> Result<Self, UciError> {
        unsafe {
            let ctx = lib.uci_alloc_context();
            if ctx.is_null() {
                return Err(UciError::AllocContext);
            }
            Ok(Self { lib, ctx })
        }
    }

    pub fn set_confdir<P: AsRef<Path>>(&mut self, path: P) -> Result<(), UciError> {
        let c_path = CString::new(path.as_ref().to_string_lossy().as_bytes())?;
        unsafe {
            let ret = self.lib.uci_set_confdir(self.ctx, c_path.as_ptr());
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    pub fn set_savedir<P: AsRef<Path>>(&mut self, path: P) -> Result<(), UciError> {
        let c_path = CString::new(path.as_ref().to_string_lossy().as_bytes())?;
        unsafe {
            let ret = self.lib.uci_set_savedir(self.ctx, c_path.as_ptr());
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    pub fn get(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
    ) -> Result<Option<String>, UciError> {
        let path_str = format!("{package}.{section}.{option}");
        let mut buf = path_str.into_bytes();
        buf.push(0); // Null terminator for in-place tokenization

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                return Ok(None);
            }
            if ptr.o.is_null() {
                return Ok(None);
            }

            let opt = &*ptr.o;
            match opt.type_ {
                uci_option_type::UCI_TYPE_STRING => {
                    let str_ptr = opt.v.string;
                    if str_ptr.is_null() {
                        Ok(None)
                    } else {
                        Ok(Some(CStr::from_ptr(str_ptr).to_string_lossy().into_owned()))
                    }
                }
                uci_option_type::UCI_TYPE_LIST => {
                    let mut values = Vec::new();
                    let head = &opt.v.list;
                    let mut curr = head.next;
                    while curr != (head as *const uci_list as *mut uci_list) && !curr.is_null() {
                        let elem = curr as *mut uci_element;
                        if !(*elem).name.is_null() {
                            values
                                .push(CStr::from_ptr((*elem).name).to_string_lossy().into_owned());
                        }
                        curr = (*curr).next;
                    }
                    Ok(Some(values.join(" ")))
                }
            }
        }
    }

    /// Read an option and parse directly into a typed primitive (e.g. u32, bool, IpAddr)
    pub fn get_parsed<T: FromStr>(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
    ) -> Result<Option<T>, UciError> {
        match self.get(package, section, option)? {
            Some(val) => val
                .parse::<T>()
                .map(Some)
                .map_err(|_| UciError::InvalidParameter(format!("Failed to parse value '{val}'"))),
            None => Ok(None),
        }
    }

    pub fn set(
        &mut self,
        package: &str,
        section: &str,
        option: &str,
        value: &str,
    ) -> Result<(), UciError> {
        let path_str = format!("{package}.{section}.{option}={value}");
        let mut buf = path_str.into_bytes();
        buf.push(0);

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            let ret = self.lib.uci_set(self.ctx, &mut ptr);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            let ret = self.lib.uci_save(self.ctx, ptr.p);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    pub fn delete(
        &mut self,
        package: &str,
        section: &str,
        option: Option<&str>,
    ) -> Result<(), UciError> {
        let path_str = match option {
            Some(opt) => format!("{package}.{section}.{opt}"),
            None => format!("{package}.{section}"),
        };
        let mut buf = path_str.into_bytes();
        buf.push(0);

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                return Ok(()); // Already absent
            }

            let ret = self.lib.uci_delete(self.ctx, &mut ptr);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }

            let ret = self.lib.uci_save(self.ctx, ptr.p);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    pub fn commit(&mut self, package: &str) -> Result<(), UciError> {
        let c_pkg = CString::new(package)?;
        let mut p: *mut uci_package = std::ptr::null_mut();

        unsafe {
            let ret = self.lib.uci_load(self.ctx, c_pkg.as_ptr(), &mut p);
            if ret != UCI_OK || p.is_null() {
                return Err(UciError::PackageNotFound(package.to_string()));
            }

            let ret = self.lib.uci_commit(self.ctx, &mut p, false);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    pub fn revert(&mut self, package: &str) -> Result<(), UciError> {
        let mut buf = package.as_bytes().to_vec();
        buf.push(0);

        let mut ptr = uci_ptr::default();

        unsafe {
            let ret =
                self.lib
                    .uci_lookup_ptr(self.ctx, &mut ptr, buf.as_mut_ptr() as *mut c_char, true);
            if ret != UCI_OK {
                return Ok(());
            }

            let ret = self.lib.uci_revert(self.ctx, &mut ptr);
            if ret != UCI_OK {
                return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
            }
        }
        Ok(())
    }

    /// Export a loaded UCI package into canonical OpenWrt UCI text format.
    pub fn export_package(&mut self, package: &str) -> Result<String, UciError> {
        let c_pkg = CString::new(package)?;
        let mut p: *mut uci_package = std::ptr::null_mut();

        unsafe {
            let ret = self.lib.uci_load(self.ctx, c_pkg.as_ptr(), &mut p);
            if ret != UCI_OK && ret != UCI_ERR_DUPLICATE {
                return Err(UciError::PackageNotFound(package.to_string()));
            }
            if p.is_null() {
                let mut ptr = uci_ptr::default();
                let mut buf = package.as_bytes().to_vec();
                buf.push(0);
                if self.lib.uci_lookup_ptr(
                    self.ctx,
                    &mut ptr,
                    buf.as_mut_ptr() as *mut c_char,
                    true,
                ) == UCI_OK
                {
                    p = ptr.p;
                }
            }
            if p.is_null() {
                return Err(UciError::PackageNotFound(package.to_string()));
            }

            #[cfg(unix)]
            {
                let mut buf_ptr: *mut c_char = std::ptr::null_mut();
                let mut buf_size: libc::size_t = 0;
                let stream = libc::open_memstream(&mut buf_ptr, &mut buf_size);
                if stream.is_null() {
                    return Err(UciError::Backend("open_memstream failed".into()));
                }

                let ret = self.lib.uci_export(self.ctx, stream, p, true);
                libc::fclose(stream);

                if ret != UCI_OK {
                    if !buf_ptr.is_null() {
                        libc::free(buf_ptr as *mut std::ffi::c_void);
                    }
                    return Err(UciError::Backend(self.lib.get_error_message(self.ctx)));
                }

                if buf_ptr.is_null() {
                    return Ok(String::new());
                }

                let res = CStr::from_ptr(buf_ptr).to_string_lossy().into_owned();
                libc::free(buf_ptr as *mut std::ffi::c_void);
                Ok(res)
            }

            #[cfg(not(unix))]
            {
                Err(UciError::Backend(
                    "export_package not supported on non-unix target".into(),
                ))
            }
        }
    }
}

impl Drop for Uci {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            unsafe {
                self.lib.uci_free_context(self.ctx);
            }
        }
    }
}
