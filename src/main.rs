#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![cfg_attr(all(feature = "axstd", target_arch = "riscv64"), feature(riscv_ext_intrinsics))]

#[cfg(feature = "axstd")]
extern crate axstd as std;

#[cfg(feature = "axstd")]
extern crate alloc;

#[cfg(feature = "axstd")]
#[macro_use]
extern crate axlog;

#[cfg(feature = "axstd")]
extern crate axfs;
#[cfg(feature = "axstd")]
extern crate axio;

// ────────────────── RISC-V 64 specific modules ──────────────────
#[cfg(all(feature = "axstd", target_arch = "riscv64"))]
mod vcpu;
#[cfg(all(feature = "axstd", target_arch = "riscv64"))]
mod regs;
#[cfg(all(feature = "axstd", target_arch = "riscv64"))]
mod csrs;
#[cfg(all(feature = "axstd", target_arch = "riscv64"))]
mod sbi;

// ────────────────── AArch64 specific modules ──────────────────
#[cfg(all(feature = "axstd", target_arch = "aarch64"))]
#[path = "aarch64/mod.rs"]
mod aarch64;

// ────────────────── x86_64 (AMD SVM) specific modules ──────────────────
#[cfg(all(feature = "axstd", target_arch = "x86_64"))]
#[path = "x86_64/mod.rs"]
mod x86_64_svm;

// ────────────────── Common modules ──────────────────
#[cfg(feature = "axstd")]
mod loader;

// VM entry point (guest physical / intermediate-physical address)
#[cfg(all(feature = "axstd", target_arch = "riscv64"))]
const VM_ENTRY: usize = 0x8020_0000;

#[cfg(all(feature = "axstd", target_arch = "aarch64"))]
const VM_ENTRY: usize = 0x4020_0000;

#[cfg(all(feature = "axstd", target_arch = "x86_64"))]
const VM_ENTRY: usize = 0x10000;

#[cfg(all(
    feature = "axstd",
    not(any(target_arch = "riscv64", target_arch = "aarch64", target_arch = "x86_64"))
))]
const VM_ENTRY: usize = 0x8020_0000;

// ════════════════════════════════════════════════════════════════
//  Entry point
// ════════════════════════════════════════════════════════════════

#[cfg_attr(feature = "axstd", unsafe(no_mangle))]
fn main() {
    #[cfg(all(feature = "axstd", target_arch = "riscv64"))]
    riscv64_main();

    #[cfg(all(feature = "axstd", target_arch = "aarch64"))]
    aarch64_main();

    #[cfg(all(feature = "axstd", target_arch = "x86_64"))]
    x86_64_main();

    #[cfg(not(feature = "axstd"))]
    {
        println!("This application requires the 'axstd' feature for running the Hypervisor.");
        println!("Run with: cargo xtask run [--arch riscv64|aarch64|x86_64]");
    }
}

// ════════════════════════════════════════════════════════════════
//  RISC-V 64  (H-extension hypervisor — h_2_0 style)
// ════════════════════════════════════════════════════════════════

#[cfg(all(feature = "axstd", target_arch = "riscv64"))]
fn riscv64_main() {
    use vcpu::VmCpuRegisters;
    use riscv::register::scause;
    use csrs::defs::hstatus;
    use tock_registers::LocalRegisterCopy;
    use csrs::{RiscvCsrTrait, CSR};
    use vcpu::_run_guest;
    use sbi::SbiMessage;
    use loader::load_vm_image;
    use axhal::mem::PhysAddr;
    use axhal::paging::MappingFlags;
    use memory_addr::va;

    ax_println!("Hypervisor ...");

    // ── 1. Create large address space (h_2_0: 0x0 .. 0x7fff_ffff_f000) ──
    let mut uspace = axmm::AddrSpace::new_empty(va!(0x0), 0x7fff_ffff_f000).unwrap();

    // Copy kernel page table entries so kernel code is accessible.
    uspace
        .copy_mappings_from(&axmm::kernel_aspace().lock())
        .unwrap();

    // ── 2. Load guest binary from disk ──
    if let Err(e) = load_vm_image("/sbin/gkernel", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // ── 3. Setup guest context ──
    let mut ctx = VmCpuRegisters::default();
    prepare_guest_context(&mut ctx);

    // ── 4. Setup second-stage page table ──
    let ept_root = uspace.page_table_root();
    prepare_vm_pgtable(ept_root);

    // ── 5. Run guest in loop (h_2_0 style) ──
    ax_println!("Entering VM run loop...");
    loop {
        unsafe {
            _run_guest(&mut ctx);
        }

        let scause = scause::read();

        if scause.is_exception() && scause.code() == 10 {
            // VirtualSupervisorEnvCall — parse SBI message
            let sbi_msg = SbiMessage::from_regs(ctx.guest_regs.gprs.a_regs()).ok();
            if let Some(msg) = sbi_msg {
                match msg {
                    SbiMessage::Reset(_) => {
                        ax_println!("VmExit Reason: VSuperEcall: {:?}", Some(&msg));
                        ax_println!("Shutdown vm normally!");
                        break;
                    }
                    _ => {
                        // Handle other SBI calls: advance guest PC by 4
                        ctx.guest_regs.sepc += 4;
                    }
                }
            } else {
                panic!("bad sbi message!");
            }
        } else if scause.is_exception() && (scause.code() == 21 || scause.code() == 23) {
            // LoadGuestPageFault (21) / StoreGuestPageFault (23)
            // — Nested Page Fault handling (h_2_0 style)
            let htval: usize;
            let stval_val: usize;
            unsafe {
                core::arch::asm!("csrr {}, htval", out(reg) htval);
                core::arch::asm!("csrr {}, stval", out(reg) stval_val);
            }
            let fault_addr = (htval << 2) | (stval_val & 0x3);
            ax_println!("VmExit: NestedPageFault addr={:#x}", fault_addr);

            // Map the faulting page with passthrough (GPA → HPA identity mapping)
            let flags = MappingFlags::READ | MappingFlags::WRITE
                | MappingFlags::EXECUTE | MappingFlags::USER;
            let _ = uspace.map_linear(
                fault_addr.into(),
                PhysAddr::from(fault_addr),
                4096,
                flags,
            );

            // Flush guest TLB
            unsafe {
                core::arch::riscv64::hfence_gvma_all();
            }
        } else {
            panic!(
                "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                scause.cause(),
                ctx.guest_regs.sepc,
                ctx.trap_csrs.stval
            );
        }
    }

    panic!("Hypervisor ok!");

    fn prepare_vm_pgtable(ept_root: PhysAddr) {
        let hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
        unsafe {
            core::arch::asm!(
                "csrw hgatp, {hgatp}",
                hgatp = in(reg) hgatp,
            );
            core::arch::riscv64::hfence_gvma_all();
        }
    }

    fn prepare_guest_context(ctx: &mut VmCpuRegisters) {
        let hstatus_val: usize;
        unsafe {
            core::arch::asm!("csrr {}, hstatus", out(reg) hstatus_val);
        }
        let mut hstatus_reg = LocalRegisterCopy::<usize, hstatus::Register>::new(hstatus_val);
        hstatus_reg.modify(hstatus::spv::Guest);
        hstatus_reg.modify(hstatus::spvp::Supervisor);
        CSR.hstatus.write_value(hstatus_reg.get());
        ctx.guest_regs.hstatus = hstatus_reg.get();

        unsafe {
            riscv::register::sstatus::set_spp(riscv::register::sstatus::SPP::Supervisor);
        }
        let sstatus_val: usize;
        unsafe {
            core::arch::asm!("csrr {}, sstatus", out(reg) sstatus_val);
        }
        ctx.guest_regs.sstatus = sstatus_val;
        ctx.guest_regs.sepc = VM_ENTRY;
    }
}

// ════════════════════════════════════════════════════════════════
//  AArch64  (EL2 hypervisor — guest at EL0, loop with page faults)
// ════════════════════════════════════════════════════════════════

#[cfg(all(feature = "axstd", target_arch = "aarch64"))]
fn aarch64_main() {
    use alloc::sync::Arc;
    use aarch64::vcpu::VmCpuRegisters;
    use loader::load_vm_image;
    use memory_addr::va;
    use axhal::paging::{MappingFlags, PageSize};
    use axmm::backend::{Backend, SharedPages};
    use memory_addr::PAGE_SIZE_4K;

    ax_println!("Hypervisor ...");

    // ── 1. Create guest address space ──
    let mut uspace = axmm::AddrSpace::new_empty(va!(0x4000_0000), 0x800_0000).unwrap();

    // ── 2. Load guest binary ──
    if let Err(e) = load_vm_image("/sbin/gkernel", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // ── 3. Switch TTBR0_EL1 to guest page table ──
    let pt_root = uspace.page_table_root();
    let new_ttbr0: u64 = usize::from(pt_root) as u64;
    let old_ttbr0: u64;
    unsafe {
        core::arch::asm!("mrs {}, ttbr0_el1", out(reg) old_ttbr0);
        core::arch::asm!(
            "msr ttbr0_el1, {val}",
            "isb",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            val = in(reg) new_ttbr0,
        );
    }

    // ── 4. Prepare guest context ──
    let mut ctx = VmCpuRegisters::default();
    ctx.guest.elr = VM_ENTRY as u64;
    ctx.guest.spsr = 0x3C0; // EL0t, DAIF masked

    // ── 5. Run guest in loop (h_2_0 style) ──
    ax_println!("Entering VM run loop...");
    loop {
        unsafe {
            aarch64::vcpu::_run_guest(&mut ctx);
        }

        let esr = ctx.trap.esr;
        let ec = (esr >> 26) & 0x3F;

        match ec {
            0x15 => {
                // SVC from EL0
                let fid = ctx.guest.gprs.0[0]; // x0 = function ID
                if fid == 0x84000008 {
                    ax_println!("VmExit Reason: SVC: PSCI SYSTEM_OFF");
                    ax_println!("Shutdown vm normally!");
                    break;
                } else {
                    ax_println!("VmExit: SVC unknown function {:#x}", fid);
                    ctx.guest.elr += 4;
                }
            }
            0x24 | 0x25 => {
                // Data abort from lower EL (0x24) or same EL (0x25)
                let far = ctx.trap.far;
                let page_addr = far & !0xFFF;
                ax_println!("VmExit: DataAbort addr={:#x}", far);

                // Map the faulting page with allocated memory
                let flags = MappingFlags::READ | MappingFlags::WRITE
                    | MappingFlags::EXECUTE | MappingFlags::USER;
                let pages = Arc::new(
                    SharedPages::new(PAGE_SIZE_4K, PageSize::Size4K)
                        .expect("alloc page for NPF"),
                );
                let _ = uspace.map(
                    (page_addr as usize).into(),
                    PAGE_SIZE_4K,
                    flags,
                    true,
                    Backend::new_shared((page_addr as usize).into(), pages),
                );

                // Flush TLB
                unsafe {
                    core::arch::asm!(
                        "tlbi vmalle1is",
                        "dsb ish",
                        "isb",
                    );
                }
            }
            _ => {
                ax_println!(
                    "Unhandled trap: EC={:#x}, ESR={:#x}, ELR={:#x}, FAR={:#x}",
                    ec, esr, ctx.guest.elr, ctx.trap.far
                );
                break;
            }
        }
    }

    // ── 6. Restore TTBR0_EL1 ──
    unsafe {
        core::arch::asm!(
            "msr ttbr0_el1, {val}",
            "isb",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            val = in(reg) old_ttbr0,
        );
    }

    ax_println!("Hypervisor ok!");
    // Shutdown QEMU via PSCI SYSTEM_OFF (SMC at EL3)
    unsafe {
        core::arch::asm!(
            "movz x0, #0x0008",
            "movk x0, #0x8400, lsl #16",
            "smc  #0",
            options(noreturn)
        );
    }
}

// ════════════════════════════════════════════════════════════════
//  x86_64  (AMD SVM hypervisor — real-mode guest, loop with NPF)
// ════════════════════════════════════════════════════════════════

#[cfg(all(feature = "axstd", target_arch = "x86_64"))]
fn x86_64_main() {
    use alloc::boxed::Box;
    use alloc::sync::Arc;
    use x86_64_svm::vmcb::*;
    use x86_64_svm::svm::*;
    use loader::load_vm_image;
    use memory_addr::va;
    use axhal::paging::{MappingFlags, PageSize};
    use axmm::backend::{Backend, SharedPages};
    use memory_addr::PAGE_SIZE_4K;

    ax_println!("Hypervisor ...");

    // ── 1. Check AMD SVM support ──
    let (_, _, ecx, _) = unsafe { cpuid(0x8000_0001) };
    if ecx & (1 << 2) == 0 {
        panic!("CPU does not support AMD SVM!");
    }

    // ── 2. Enable SVM ──
    unsafe {
        let efer = rdmsr(MSR_EFER);
        wrmsr(MSR_EFER, efer | EFER_SVME);
    }

    // ── 3. Allocate host-save area ──
    #[repr(C, align(4096))]
    struct Page4K([u8; 4096]);
    let host_save = Box::new(Page4K([0u8; 4096]));
    let host_save_pa = virt_to_phys_ptr(&host_save.0[0]);
    unsafe {
        wrmsr(MSR_VM_HSAVE_PA, host_save_pa);
    }

    // Host VMCB for FS/GS/TR/LDTR save/restore
    let host_vmcb = Box::new(Page4K([0u8; 4096]));
    let host_vmcb_pa = virt_to_phys_ptr(&host_vmcb.0[0]);

    // ── 4. Allocate IOPM and MSRPM ──
    #[repr(C, align(4096))]
    struct Iopm([u8; 12288]);
    #[repr(C, align(4096))]
    struct Msrpm([u8; 8192]);
    let iopm = Box::new(Iopm([0u8; 12288]));
    let msrpm = Box::new(Msrpm([0u8; 8192]));
    let iopm_pa = virt_to_phys_ptr(&iopm.0[0]);
    let msrpm_pa = virt_to_phys_ptr(&msrpm.0[0]);

    // ── 5. Create NPT and load guest binary ──
    let mut npt = axmm::AddrSpace::new_empty(va!(VM_ENTRY), 0x100_0000).unwrap();
    if let Err(e) = load_vm_image("/sbin/gkernel", &mut npt) {
        panic!("Cannot load app! {:?}", e);
    }
    let npt_root_pa: u64 = usize::from(npt.page_table_root()) as u64;

    // ── 6. Build VMCB ──
    let mut vmcb = Box::new(Vmcb::new());

    // Control area — intercept VMRUN, VMMCALL, and NPF
    vmcb.write_u32(CTRL_INTERCEPT_MISC2, INTERCEPT_VMRUN | INTERCEPT_VMMCALL);
    vmcb.write_u64(CTRL_IOPM_BASE, iopm_pa);
    vmcb.write_u64(CTRL_MSRPM_BASE, msrpm_pa);
    vmcb.write_u32(CTRL_GUEST_ASID, 1);
    vmcb.write_u64(CTRL_NP_ENABLE, 1);
    vmcb.write_u64(CTRL_NCR3, npt_root_pa);

    // Save area — 16-bit real-mode guest
    vmcb.set_segment(SAVE_CS, (VM_ENTRY >> 4) as u16, 0x009B, 0xFFFF, VM_ENTRY as u64);
    vmcb.set_segment(SAVE_DS, 0, 0x0093, 0xFFFF, 0);
    vmcb.set_segment(SAVE_ES, 0, 0x0093, 0xFFFF, 0);
    vmcb.set_segment(SAVE_SS, 0, 0x0093, 0xFFFF, 0);
    vmcb.set_segment(SAVE_FS, 0, 0x0093, 0xFFFF, 0);
    vmcb.set_segment(SAVE_GS, 0, 0x0093, 0xFFFF, 0);
    vmcb.set_segment(SAVE_GDTR, 0, 0, 0xFFFF, 0);
    vmcb.set_segment(SAVE_IDTR, 0, 0, 0x3FF, 0);
    vmcb.set_segment(SAVE_TR, 0, 0x008B, 0xFFFF, 0);
    vmcb.set_segment(SAVE_LDTR, 0, 0x0082, 0, 0);

    vmcb.write_u64(SAVE_EFER, EFER_SVME);
    vmcb.write_u64(SAVE_CR0, 0x10);
    vmcb.write_u64(SAVE_DR6, 0xFFFF_0FF0);
    vmcb.write_u64(SAVE_DR7, 0x0400);
    vmcb.write_u64(SAVE_RFLAGS, 0x2);
    vmcb.write_u64(SAVE_RIP, 0);

    let vmcb_pa = virt_to_phys_ptr(&vmcb.data[0]);
    ax_println!("paddr: PA:{:#x}", vmcb_pa);

    // ── 7. Run guest in loop (h_2_0 style) ──
    ax_println!("Entering VM run loop...");
    loop {
        unsafe {
            _run_guest(vmcb_pa, host_vmcb_pa);
        }

        let exit_code = vmcb.exit_code();

        match exit_code {
            VMEXIT_VMMCALL => {
                let guest_rax = vmcb.guest_rax();
                if guest_rax == 0x84000008 {
                    ax_println!("VmExit Reason: VMMCALL");
                    ax_println!("Shutdown vm normally!");
                    break;
                } else {
                    ax_println!("VmExit: VMMCALL unknown function {:#x}", guest_rax);
                    // Advance guest RIP past VMMCALL (3 bytes)
                    let rip = vmcb.guest_rip();
                    vmcb.write_u64(SAVE_RIP, rip + 3);
                }
            }
            VMEXIT_NPF => {
                let fault_addr = vmcb.exit_info2();
                let page_addr = (fault_addr & !0xFFF) as usize;
                ax_println!("VmExit: NPF addr={:#x}", fault_addr);

                // Map the faulting page in NPT with allocated memory
                let flags = MappingFlags::READ | MappingFlags::WRITE
                    | MappingFlags::EXECUTE | MappingFlags::USER;
                let pages = Arc::new(
                    SharedPages::new(PAGE_SIZE_4K, PageSize::Size4K)
                        .expect("alloc page for NPF"),
                );
                let _ = npt.map(
                    page_addr.into(),
                    PAGE_SIZE_4K,
                    flags,
                    true,
                    Backend::new_shared(page_addr.into(), pages),
                );
                // NPT is re-walked on next VMRUN, no explicit flush needed
            }
            _ => {
                ax_println!(
                    "Unexpected VMEXIT: exit_code={:#x}, info1={:#x}, info2={:#x}, RIP={:#x}",
                    exit_code,
                    vmcb.exit_info1(),
                    vmcb.exit_info2(),
                    vmcb.guest_rip(),
                );
                break;
            }
        }
    }

    ax_println!("Hypervisor ok!");

    // Shutdown QEMU via ACPI
    unsafe {
        core::arch::asm!(
            "mov dx, 0x604",
            "mov ax, 0x2000",
            "out dx, ax",
        );
    }
    panic!("Hypervisor ok!");

    fn virt_to_phys_ptr(p: *const u8) -> u64 {
        use axhal::mem::virt_to_phys;
        let va = memory_addr::VirtAddr::from(p as usize);
        usize::from(virt_to_phys(va)) as u64
    }
}
