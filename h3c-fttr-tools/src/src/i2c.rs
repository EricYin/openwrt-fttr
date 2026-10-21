use std::io;

#[derive(Clone, Copy)]
pub(crate) enum Register {
    TxDis,
}

impl Register {
    const fn address(self) -> u8 {
        match self {
            Self::TxDis => 0x14,
        }
    }

    const fn write_payload(self, value: u8) -> [u8; 2] {
        [self.address(), value]
    }
}

pub(crate) fn read_register(register: Register) -> io::Result<u8> {
    #[cfg(target_os = "linux")]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/i2c-5")?;
        let mut address = [register.address()];
        let mut value = [0];
        let mut messages = encoding::read_messages(&mut address, &mut value);
        transfer(&file, &mut messages)?;
        Ok(value[0])
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = register;
        Err(io::ErrorKind::Unsupported.into())
    }
}

pub(crate) fn write_register(register: Register, value: u8) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/i2c-5")?;
        let mut payload = register.write_payload(value);
        let mut messages = encoding::write_messages(&mut payload);
        transfer(&file, &mut messages)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (register, value);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(any(target_os = "linux", test))]
mod encoding {
    use std::marker::PhantomData;

    // Stock 0xB0 is the 8-bit write address, not the Linux 7-bit address.
    const SLAVE: u16 = 0x58;
    const I2C_M_RD: u16 = 0x0001;

    #[repr(C)]
    pub(super) struct Message<'a> {
        pub(super) addr: u16,
        pub(super) flags: u16,
        pub(super) len: u16,
        pub(super) buf: *mut u8,
        lifetime: PhantomData<&'a mut [u8]>,
    }

    pub(super) fn read_messages<'a>(
        register: &'a mut [u8; 1],
        value: &'a mut [u8; 1],
    ) -> [Message<'a>; 2] {
        [
            Message {
                addr: SLAVE,
                flags: 0,
                len: 1,
                buf: register.as_mut_ptr(),
                lifetime: PhantomData,
            },
            Message {
                addr: SLAVE,
                flags: I2C_M_RD,
                len: 1,
                buf: value.as_mut_ptr(),
                lifetime: PhantomData,
            },
        ]
    }

    pub(super) fn write_messages(payload: &mut [u8; 2]) -> [Message<'_>; 1] {
        [Message {
            addr: SLAVE,
            flags: 0,
            len: 2,
            buf: payload.as_mut_ptr(),
            lifetime: PhantomData,
        }]
    }
}

#[cfg(target_os = "linux")]
fn transfer(file: &std::fs::File, messages: &mut [encoding::Message<'_>]) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    #[repr(C)]
    struct RdwrData<'a> {
        msgs: *mut encoding::Message<'a>,
        nmsgs: u32,
    }

    let count = u32::try_from(messages.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut data = RdwrData {
        msgs: messages.as_mut_ptr(),
        nmsgs: count,
    };
    // SAFETY: Linux I2C_RDWR uses these repr(C) UAPI layouts (u16 fields,
    // pointer, u32 count). Message constructors borrow initialized, exclusive
    // buffers of exactly len bytes; those buffers, messages, data and file stay
    // live throughout this synchronous ioctl, which retains no pointers.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), 0x0707, &mut data) };
    let result = if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    };
    check_completion(result, count)
}

#[cfg(any(target_os = "linux", test))]
fn check_completion(result: io::Result<i32>, expected: u32) -> io::Result<()> {
    let completed = result?;
    if i64::from(completed) != i64::from(expected) {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_uses_register_write_then_read_when_encoded() {
        let mut register = [Register::TxDis.address()];
        let mut value = [0];
        let register_ptr = register.as_mut_ptr();
        let value_ptr = value.as_mut_ptr();

        let messages = encoding::read_messages(&mut register, &mut value);

        assert_eq!(
            (messages[0].addr, messages[0].flags, messages[0].len),
            (0x58, 0, 1)
        );
        assert_eq!(
            (messages[1].addr, messages[1].flags, messages[1].len),
            (0x58, 1, 1)
        );
        assert_eq!(messages[0].buf, register_ptr);
        assert_eq!(messages[1].buf, value_ptr);
        assert_eq!(register, [0x14]);
    }

    #[test]
    fn write_sends_register_and_value_when_encoded() {
        let mut payload = Register::TxDis.write_payload(0xa5);
        let payload_ptr = payload.as_mut_ptr();

        let messages = encoding::write_messages(&mut payload);

        assert_eq!(
            (messages[0].addr, messages[0].flags, messages[0].len),
            (0x58, 0, 2)
        );
        assert_eq!(messages[0].buf, payload_ptr);
        assert_eq!(payload, [0x14, 0xa5]);
    }

    #[test]
    fn completion_succeeds_when_all_messages_transfer() {
        let results = [(Ok(1), 1), (Ok(2), 2)];

        for (result, expected) in results {
            assert!(check_completion(result, expected).is_ok());
        }
    }

    #[test]
    fn completion_fails_when_transfer_is_incomplete() {
        let results = [(0, 1), (0, 2), (1, 2)];

        for (completed, expected) in results {
            let error = check_completion(Ok(completed), expected).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        }
    }

    #[test]
    fn completion_preserves_error_when_kernel_fails() {
        let result = Err(io::Error::from_raw_os_error(6));

        let error = check_completion(result, 2).unwrap_err();

        assert_eq!(error.raw_os_error(), Some(6));
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn register_access_fails_when_platform_is_unsupported() {
        let register = Register::TxDis;

        let read = read_register(register);
        let write = write_register(register, 0xa5);

        assert_eq!(read.unwrap_err().kind(), io::ErrorKind::Unsupported);
        assert_eq!(write.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }
}
