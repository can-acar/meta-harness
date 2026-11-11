//! Misafir fiziksel belleği: tek parça, GPA 0'dan başlar (Unix: anonim mmap, Windows: VirtualAlloc).

use std::io;
use std::sync::atomic::{fence, Ordering};

pub struct GuestMem {
    ptr: *mut u8,
    size: usize,
}

// Bellek bölgesi vCPU iş parçacıkları ve aygıtlar arasında paylaşılır;
// eşzamanlılık virtio halka protokolü ve bellek çitleriyle sağlanır.
unsafe impl Send for GuestMem {}
unsafe impl Sync for GuestMem {}

impl GuestMem {
    pub fn new(size: usize) -> io::Result<Self> {
        let ptr = sys::alloc(size)?;
        Ok(GuestMem { ptr, size })
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn host_addr(&self) -> u64 {
        self.ptr as u64
    }

    fn check(&self, gpa: u64, len: usize) -> Option<usize> {
        let start = usize::try_from(gpa).ok()?;
        let end = start.checked_add(len)?;
        (end <= self.size).then_some(start)
    }

    /// Misafir belleğinin bir kesitine ham erişim. Çağıran, aynı anda
    /// misafirin bu bölgeye yazabileceğini bilerek kullanmalıdır.
    #[allow(clippy::mut_from_ref)]
    pub fn slice_mut(&self, gpa: u64, len: usize) -> Option<&mut [u8]> {
        let off = self.check(gpa, len)?;
        Some(unsafe { std::slice::from_raw_parts_mut(self.ptr.add(off), len) })
    }

    pub fn write(&self, gpa: u64, data: &[u8]) -> bool {
        match self.slice_mut(gpa, data.len()) {
            Some(s) => {
                s.copy_from_slice(data);
                true
            }
            None => false,
        }
    }

    pub fn read(&self, gpa: u64, out: &mut [u8]) -> bool {
        match self.slice_mut(gpa, out.len()) {
            Some(s) => {
                out.copy_from_slice(s);
                true
            }
            None => false,
        }
    }

    pub fn read_obj<T: Copy>(&self, gpa: u64) -> Option<T> {
        let off = self.check(gpa, std::mem::size_of::<T>())?;
        Some(unsafe { std::ptr::read_volatile(self.ptr.add(off) as *const T) })
    }

    pub fn write_obj<T: Copy>(&self, gpa: u64, v: T) -> bool {
        match self.check(gpa, std::mem::size_of::<T>()) {
            Some(off) => {
                unsafe { std::ptr::write_volatile(self.ptr.add(off) as *mut T, v) };
                true
            }
            None => false,
        }
    }

    pub fn u16_at(&self, gpa: u64) -> Option<u16> {
        let off = self.check(gpa, 2)?;
        Some(u16::from_le(unsafe { std::ptr::read_unaligned(self.ptr.add(off) as *const u16) }))
    }
    pub fn u32_at(&self, gpa: u64) -> Option<u32> {
        let off = self.check(gpa, 4)?;
        Some(u32::from_le(unsafe { std::ptr::read_unaligned(self.ptr.add(off) as *const u32) }))
    }
    pub fn u64_at(&self, gpa: u64) -> Option<u64> {
        let off = self.check(gpa, 8)?;
        Some(u64::from_le(unsafe { std::ptr::read_unaligned(self.ptr.add(off) as *const u64) }))
    }
    pub fn set_u16(&self, gpa: u64, v: u16) -> bool {
        self.write(gpa, &v.to_le_bytes())
    }
    pub fn set_u32(&self, gpa: u64, v: u32) -> bool {
        self.write(gpa, &v.to_le_bytes())
    }
    pub fn set_u64(&self, gpa: u64, v: u64) -> bool {
        self.write(gpa, &v.to_le_bytes())
    }

    pub fn barrier() {
        fence(Ordering::SeqCst);
    }
}

impl Drop for GuestMem {
    fn drop(&mut self) {
        unsafe { sys::free(self.ptr, self.size) };
    }
}

/// Platforma özgü bellek ayırma. Bölge sayfa hizalı, sıfırlanmış ve okunur/yazılır olmalı
/// (KVM, WHP ve HVF eşleme için sayfa hizası ister).
#[cfg(unix)]
mod sys {
    use std::io;

    pub fn alloc(size: usize) -> io::Result<*mut u8> {
        #[cfg(target_os = "linux")]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
        #[cfg(not(target_os = "linux"))]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, flags, -1, 0) };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(ptr as *mut u8)
    }

    pub unsafe fn free(ptr: *mut u8, size: usize) {
        libc::munmap(ptr as *mut libc::c_void, size);
    }
}

#[cfg(windows)]
mod sys {
    use std::io;
    use windows_sys::Win32::System::Memory::{VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE};

    pub fn alloc(size: usize) -> io::Result<*mut u8> {
        // VirtualAlloc 64 KiB hizalı, sıfırlanmış sayfalar verir; fiziksel sayfalar ilk dokunuşta ayrılır.
        let ptr = unsafe { VirtualAlloc(std::ptr::null(), size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
        if ptr.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(ptr as *mut u8)
    }

    pub unsafe fn free(ptr: *mut u8, _size: usize) {
        VirtualFree(ptr as *mut core::ffi::c_void, 0, MEM_RELEASE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sinirlar() {
        let m = GuestMem::new(4096).unwrap();
        assert!(m.set_u32(4092, 0xdeadbeef));
        assert_eq!(m.u32_at(4092), Some(0xdeadbeef));
        assert!(!m.set_u32(4093, 1));
        assert_eq!(m.u64_at(u64::MAX - 2), None);
    }
}
