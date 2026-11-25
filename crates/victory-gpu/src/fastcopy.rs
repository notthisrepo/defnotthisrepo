//! Copies of title memory that may be gone, made in user mode.
//!
//! The title may unmap pages of a range between the moment a copy of it is
//! recorded and the moment it is made, so a plain copy can fault. The copy
//! was therefore made by the kernel (`ReadProcessMemory` on our own
//! process), which says when a page is not there. That is a system call
//! per copy and a copy at the kernel's pace: in gameplay the submitting
//! thread copied 171 MB a frame in 71 ms (2.4 GB a second), and a sample of
//! its stacks had 16 to 20 % of its processor time inside that call.
//!
//! Here the copy is one `rep movsb` of our own, at a known address, and a
//! vectored handler: a fault on a read at that instruction is a source page
//! that is not there, and the handler sends the thread to the routine's
//! failure exit, which returns false as the kernel's copy did. A fault on a
//! write there is the destination's business (a guarded block of title
//! memory, `crate::watch`, whose handler opens it and lets the instruction
//! go on), and is left to it.
//!
//! `VICTORY_COPY_KERNEL=1` copies through the kernel, as before.

use std::sync::OnceLock;

// Win64: rcx destination, rdx source, r8 length. `rsi` and `rdi` are the
// caller's and are put back on both exits; the stack is the same at the
// move and at the failure exit, which is where the handler sends a thread
// that faulted in the move.
std::arch::global_asm!(
    ".globl victory_guarded_copy",
    ".globl victory_guarded_copy_move",
    ".globl victory_guarded_copy_fault",
    ".def victory_guarded_copy; .scl 2; .type 32; .endef",
    ".seh_proc victory_guarded_copy",
    "victory_guarded_copy:",
    "    push rsi",
    "    .seh_pushreg rsi",
    "    push rdi",
    "    .seh_pushreg rdi",
    "    .seh_endprologue",
    "    mov rdi, rcx",
    "    mov rsi, rdx",
    "    mov rcx, r8",
    "victory_guarded_copy_move:",
    "    rep movsb",
    "    mov eax, 1",
    "    pop rdi",
    "    pop rsi",
    "    ret",
    "victory_guarded_copy_fault:",
    "    xor eax, eax",
    "    pop rdi",
    "    pop rsi",
    "    ret",
    ".seh_endproc",
);

unsafe extern "C" {
    fn victory_guarded_copy(dst: *mut u8, src: *const u8, len: usize) -> u64;
    /// The move instruction, and where a thread that faulted reading in it
    /// goes on.
    static victory_guarded_copy_move: u8;
    static victory_guarded_copy_fault: u8;
}

/// Whether copies of title memory are made here and not by the kernel.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_COPY_KERNEL").is_none_or(|v| v != "1");
        if on {
            install();
        }
        on
    })
}

/// Registers the handler that turns a read fault in the move into the
/// routine's failure exit, once.
fn install() {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, EXCEPTION_IN_PAGE_ERROR, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn on_fault(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched and the
        // faulting thread's context.
        unsafe {
            let r = &*(*info).ExceptionRecord;
            let context = &mut *(*info).ContextRecord;
            if context.Rip != (&raw const victory_guarded_copy_move) as u64 {
                return EXCEPTION_CONTINUE_SEARCH;
            }
            let code = r.ExceptionCode;
            if code != EXCEPTION_ACCESS_VIOLATION as NTSTATUS && code != EXCEPTION_IN_PAGE_ERROR as NTSTATUS {
                return EXCEPTION_CONTINUE_SEARCH;
            }
            // A write: the destination is guarded title memory, which its
            // own handler opens.
            const WRITE: usize = 1;
            if r.NumberParameters >= 1 && r.ExceptionInformation[0] == WRITE {
                return EXCEPTION_CONTINUE_SEARCH;
            }
            context.Rip = (&raw const victory_guarded_copy_fault) as u64;
            EXCEPTION_CONTINUE_EXECUTION
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: the handler reads the exception it is given and changes only
    // the context of a thread inside our own routine.
    ONCE.call_once(|| unsafe {
        AddVectoredExceptionHandler(1, Some(on_fault));
    });
}

/// Copies `len` bytes from `src` to `dst`; false if a source page is not
/// readable (the destination may then hold the bytes before it).
///
/// # Safety
/// `dst` is `len` bytes of memory that may be written (guarded title memory
/// included: its handler opens it); [`on`] has returned true.
pub(crate) unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) -> bool {
    // SAFETY: the routine reads `len` bytes at `src`, where a fault is its
    // failure exit, and writes `len` bytes at `dst`, which the caller
    // vouches for.
    unsafe { victory_guarded_copy(dst, src, len) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes arrive whole, whatever the length and alignment.
    #[test]
    fn a_copy_of_memory_that_is_there_is_whole() {
        assert!(on());
        let src: Vec<u8> = (0..70_000u32).map(|i| (i * 31 + 7) as u8).collect();
        for (from, len) in [(0usize, 0usize), (0, 1), (3, 15), (1, 4096), (5, 65_537), (0, 70_000)] {
            let mut dst = vec![0xAAu8; len + 2];
            // SAFETY: both buffers hold the bytes named.
            assert!(unsafe { copy(dst.as_mut_ptr().add(1), src.as_ptr().add(from), len) });
            assert_eq!(&dst[1..=len], &src[from..from + len]);
            assert_eq!((dst[0], dst[len + 1]), (0xAA, 0xAA), "a byte outside the copy changed");
        }
    }

    /// A source with a page that is not there fails without ending the
    /// process, as the kernel's copy did, and the routine works after it.
    #[test]
    fn a_copy_from_memory_that_is_gone_fails_and_nothing_else_does() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree};
        assert!(on());
        const PAGE: usize = 4096;
        // SAFETY: two pages reserved, the first committed; released below.
        unsafe {
            let at = VirtualAlloc(std::ptr::null(), 2 * PAGE, MEM_RESERVE, PAGE_READWRITE).cast::<u8>();
            assert!(!at.is_null());
            assert!(!VirtualAlloc(at.cast(), PAGE, MEM_COMMIT, PAGE_READWRITE).is_null());
            at.write_bytes(0x5A, PAGE);
            let mut dst = vec![0u8; 2 * PAGE];
            // Inside the committed page.
            assert!(copy(dst.as_mut_ptr(), at, PAGE));
            assert!(dst[..PAGE].iter().all(|&b| b == 0x5A));
            // Across into the page that is only reserved, and wholly in it.
            assert!(!copy(dst.as_mut_ptr(), at.add(PAGE - 16), 64));
            assert!(!copy(dst.as_mut_ptr(), at.add(PAGE), 8));
            // And again where it is there.
            assert!(copy(dst.as_mut_ptr(), at.add(8), 32));
            VirtualFree(at.cast(), 0, MEM_RELEASE);
        }
    }
}
