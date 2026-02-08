#![no_std]
#![no_main]

use core::panic::PanicInfo;

/// Minimal guest kernel that triggers a Nested Page Fault, then shuts down.
///
/// On each architecture, the guest:
///   1. Accesses an unmapped address → triggers NPF / page fault
///      The hypervisor maps the page, and the CPU re-executes the instruction.
///   2. Performs a shutdown hypercall → hypervisor exits the VM loop.

// ── x86_64: 16-bit real mode via global_asm (avoids compiler prologue) ──
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".code16",
    ".global _start",
    "_start:",
    // Access GPA 0x20000 (unmapped in NPT) → triggers NPF
    "mov ebx, 0x20000",
    "mov eax, [ebx]",
    // Shutdown via VMMCALL
    "mov eax, 0x84000008",
    "vmmcall",
    "2: jmp 2b",
    ".code64",
);

// ── riscv64 / aarch64: define _start as a Rust function ──
#[cfg(not(target_arch = "x86_64"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn _start() -> ! {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!(
            // Access pflash address 0x2200_0000 (unmapped) → triggers NPF
            "li t0, 0x22000000",
            "lw t1, 0(t0)",
            // SBI legacy shutdown
            "li a7, 8",
            "ecall",
            "j .",
            options(noreturn)
        );
    }

    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            // Access address 0x4020_2000 (unmapped in guest page table) → page fault
            "movz x1, #0x2000",
            "movk x1, #0x4020, lsl #16",
            "ldr w2, [x1]",
            // PSCI SYSTEM_OFF
            "movz x0, #0x0008",
            "movk x0, #0x8400, lsl #16",
            "svc #0",
            "b .",
            options(noreturn)
        );
    }

    #[cfg(not(any(target_arch = "riscv64", target_arch = "aarch64")))]
    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
