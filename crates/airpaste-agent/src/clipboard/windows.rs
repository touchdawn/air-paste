use std::{ffi::OsStr, iter, os::windows::ffi::OsStrExt, path::PathBuf, ptr};
use windows_sys::Win32::{
    Foundation::HWND,
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
            OpenClipboard, SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        Ole::{CF_HDROP, CF_UNICODETEXT},
    },
    UI::Shell::{DragQueryFileW, HDROP},
};

pub struct Clipboard;

impl Clipboard {
    pub fn new() -> Self {
        Self
    }

    pub fn get_text(&self) -> anyhow::Result<Option<String>> {
        let _guard = ClipboardGuard::open()?;
        let available = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.into()) != 0 };
        if !available {
            return Ok(None);
        }

        let handle = unsafe { GetClipboardData(CF_UNICODETEXT.into()) };
        if handle.is_null() {
            return Ok(None);
        }

        let ptr = unsafe { GlobalLock(handle) } as *const u16;
        if ptr.is_null() {
            return Ok(None);
        }

        let text = unsafe {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            String::from_utf16_lossy(slice)
        };
        unsafe {
            GlobalUnlock(handle);
        }
        Ok(Some(text))
    }

    pub fn set_text(&self, text: &str) -> anyhow::Result<()> {
        let _guard = ClipboardGuard::open()?;
        let utf16: Vec<u16> = OsStr::new(text)
            .encode_wide()
            .chain(iter::once(0))
            .collect();
        let byte_len = utf16.len() * std::mem::size_of::<u16>();
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) };
        if handle.is_null() {
            anyhow::bail!("GlobalAlloc failed");
        }

        let ptr = unsafe { GlobalLock(handle) } as *mut u8;
        if ptr.is_null() {
            anyhow::bail!("GlobalLock failed");
        }
        unsafe {
            ptr::copy_nonoverlapping(utf16.as_ptr() as *const u8, ptr, byte_len);
            GlobalUnlock(handle);
        }

        let emptied = unsafe { EmptyClipboard() != 0 };
        if !emptied {
            anyhow::bail!("EmptyClipboard failed");
        }

        let set = unsafe { SetClipboardData(CF_UNICODETEXT.into(), handle) };
        if set.is_null() {
            anyhow::bail!("SetClipboardData failed");
        }
        Ok(())
    }

    pub fn get_files(&self) -> anyhow::Result<Option<Vec<PathBuf>>> {
        let _guard = ClipboardGuard::open()?;
        let available = unsafe { IsClipboardFormatAvailable(CF_HDROP.into()) != 0 };
        if !available {
            return Ok(None);
        }

        let handle = unsafe { GetClipboardData(CF_HDROP.into()) };
        if handle.is_null() {
            return Ok(None);
        }
        let hdrop = handle as HDROP;
        let count = unsafe { DragQueryFileW(hdrop, u32::MAX, ptr::null_mut(), 0) };
        if count == 0 {
            return Ok(Some(Vec::new()));
        }

        let mut files = Vec::with_capacity(count as usize);
        for index in 0..count {
            let len = unsafe { DragQueryFileW(hdrop, index, ptr::null_mut(), 0) };
            if len == 0 {
                continue;
            }
            let mut buffer = vec![0u16; len as usize + 1];
            let copied =
                unsafe { DragQueryFileW(hdrop, index, buffer.as_mut_ptr(), buffer.len() as u32) };
            if copied == 0 {
                continue;
            }
            buffer.truncate(copied as usize);
            files.push(PathBuf::from(String::from_utf16_lossy(&buffer)));
        }

        Ok(Some(files))
    }
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> anyhow::Result<Self> {
        let opened = unsafe { OpenClipboard(HWND::default()) != 0 };
        if !opened {
            anyhow::bail!("OpenClipboard failed");
        }
        Ok(Self)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}
