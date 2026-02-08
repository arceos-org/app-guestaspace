# arceos-guestaspace

A standalone hypervisor application running on [ArceOS](https://github.com/arceos-org/arceos) unikernel, with all dependencies sourced from [crates.io](https://crates.io). Implements guest address space management with **loop-based VM exit handling** and **nested page fault (NPF)** support across three architectures.

This crate is derived from the [h_2_0](https://github.com/arceos-org/arceos/tree/main/tour/h_2_0) tutorial crate in the ArceOS ecosystem, extending it to support multiple processor architectures.

## What It Does

The hypervisor (`arceos-guestaspace`) performs the following:

1. **Creates a guest address space** with second-stage/nested page tables
2. **Loads a minimal guest kernel** (`gkernel`) from a VirtIO block device
3. **Runs the guest in a loop**, handling VM exits:
   - **Nested Page Fault (NPF)**: When the guest accesses an unmapped address, the hypervisor maps the page and resumes guest execution
   - **Shutdown request**: When the guest issues a shutdown hypercall, the hypervisor exits cleanly
4. **Demonstrates the h_2_0 control flow**: loop → VMRUN → VMEXIT → handle → repeat

The guest kernel (`gkernel`) is a minimal bare-metal program that:
1. Accesses an unmapped address (triggering an NPF)
2. Performs a shutdown hypercall

## Architecture Support

| Architecture | Virtualization | Guest Mode | NPF Trigger Address | Shutdown Mechanism |
|---|---|---|---|---|
| RISC-V 64 | H-extension | VS-mode | `0x22000000` (pflash) | SBI `ecall` (Reset) |
| AArch64 | EL2 | EL0 | `0x40202000` (unmapped) | PSCI `svc` (SYSTEM_OFF) |
| x86_64 | AMD SVM | Real mode | `0x20000` (unmapped) | `vmmcall` |

## Control Flow (h_2_0 Compatible)

```
Hypervisor starts
    │
    ├─ Create guest address space (AddrSpace)
    ├─ Load guest binary from /sbin/gkernel
    ├─ Setup vCPU context
    │
    └─ VM Run Loop ──────────────────────┐
         │                               │
         ├─ Enter guest (vmrun/eret)     │
         │                               │
         ├─ Guest accesses unmapped addr │
         │   └─ NPF / Page Fault exit   │
         │       └─ Map the page ────────┘
         │
         ├─ Guest re-executes (success)
         │
         ├─ Guest issues shutdown call
         │   └─ Shutdown exit
         │       └─ Break loop
         │
         └─ "Hypervisor ok!"
```

## Comparison with Related Crates

| Crate | Role | Description |
|---|---|---|
| **arceos-guestaspace** (this) | Hypervisor | Runs guest with NPF handling (like h_2_0) |
| [arceos-guestmode](https://crates.io/crates/arceos-guestmode) | Hypervisor | Runs minimal guest, single VM exit (like h_1_0) |
| [arceos-readpflash](https://crates.io/crates/arceos-readpflash) | Guest | Reads PFlash via MMIO (like u_3_0) |

## Prerequisites

- **Rust nightly toolchain** (edition 2024)

  ```bash
  rustup install nightly
  rustup default nightly
  ```

- **Bare-metal targets**

  ```bash
  rustup target add riscv64gc-unknown-none-elf
  rustup target add aarch64-unknown-none-softfloat
  rustup target add x86_64-unknown-none
  ```

- **QEMU** (with virtualization support)

  ```bash
  # Ubuntu/Debian
  sudo apt install qemu-system-riscv64 qemu-system-aarch64 qemu-system-x86

  # macOS (Homebrew)
  brew install qemu
  ```

- **rust-objcopy** (from `cargo-binutils`)

  ```bash
  cargo install cargo-binutils
  rustup component add llvm-tools
  ```

## Quick Start

```bash
# Install cargo-clone
cargo install cargo-clone

# Get source code from crates.io
cargo clone arceos-guestaspace
cd arceos-guestaspace

# Build and run on RISC-V 64 (default)
cargo xtask run

# Build and run on other architectures
cargo xtask run --arch aarch64
cargo xtask run --arch x86_64

# Build only (no QEMU)
cargo xtask build --arch riscv64
```

## Expected Output

### RISC-V 64

```
       d8888                            .d88888b.   .d8888b.
      ...
d88P     888 888      "Y8888P  "Y8888   "Y88888P"   "Y8888P"

arch = riscv64
platform = riscv64-qemu-virt
smp = 1

Hypervisor ...
app: /sbin/gkernel
paddr: PA:0x80633000
Entering VM run loop...
VmExit: NestedPageFault addr=0x22000000
VmExit Reason: VSuperEcall: Some(Reset(...))
Shutdown vm normally!
Hypervisor ok!
```

### AArch64

```
       d8888          ...
Hypervisor ...
app: /sbin/gkernel
paddr: PA:0x404f4000
Entering VM run loop...
VmExit: DataAbort addr=0x40202000
VmExit Reason: SVC: PSCI SYSTEM_OFF
Shutdown vm normally!
Hypervisor ok!
```

### x86_64 (AMD SVM)

```
       d8888          ...
Hypervisor ...
app: /sbin/gkernel
paddr: PA:0x44e000
Entering VM run loop...
VmExit: NPF addr=0x20000
VmExit Reason: VMMCALL
Shutdown vm normally!
Hypervisor ok!
```

## Project Structure

```
app-guestaspace/
├── .cargo/
│   └── config.toml            # cargo xtask alias & AX_CONFIG_PATH
├── payload/
│   └── gkernel/               # Minimal guest kernel (triggers NPF + shutdown)
│       ├── Cargo.toml
│       └── src/main.rs
├── xtask/
│   └── src/main.rs            # Build/run tool (disk image, pflash, QEMU)
├── configs/
│   ├── riscv64.toml           # Platform config for riscv64-qemu-virt
│   ├── aarch64.toml           # Platform config for aarch64-qemu-virt
│   └── x86_64.toml            # Platform config for x86-pc
├── src/
│   ├── main.rs                # Hypervisor entry: loop-based VM exit handling
│   ├── loader.rs              # Guest binary loader (FAT32 → address space)
│   ├── vcpu.rs                # RISC-V vCPU context (registers, guest.S)
│   ├── guest.S                # RISC-V guest entry/exit assembly
│   ├── regs.rs                # RISC-V general-purpose registers
│   ├── csrs.rs                # RISC-V hypervisor CSR definitions
│   ├── sbi/                   # SBI message parsing (base, reset, fence, ...)
│   ├── aarch64/               # AArch64 EL2 vCPU, guest.S, HVC handling
│   └── x86_64/                # AMD SVM: VMCB, vmrun assembly, helpers
├── build.rs                   # Linker script auto-detection
├── Cargo.toml
├── rust-toolchain.toml
└── README.md
```

## How It Works

### `cargo xtask run --arch <ARCH>`

1. Copies `configs/<ARCH>.toml` → `.axconfig.toml`
2. Builds the guest payload (`gkernel`) for the target architecture
3. Creates a 64MB FAT32 disk image with `/sbin/gkernel`
4. For riscv64: creates a 32MB pflash image with "pfld" magic at offset 0
5. Builds the hypervisor kernel with `--features axstd`
6. Launches QEMU with VirtIO block device (and pflash for riscv64)

### VM Exit Handling

| Architecture | NPF Exit | NPF Address Source | Shutdown Exit |
|---|---|---|---|
| RISC-V 64 | `scause` = 21/23 | `htval << 2 \| stval & 3` | `scause` = 10 (VSupervisorEnvCall) + SBI Reset |
| AArch64 | ESR EC = 0x24 | `FAR` register | ESR EC = 0x15 (SVC) + PSCI SYSTEM_OFF |
| x86_64 SVM | VMEXIT 0x400 | VMCB EXITINFO2 | VMEXIT 0x81 (VMMCALL) |

### QEMU Configuration

| Architecture | QEMU Command | Special Options |
|---|---|---|
| riscv64 | `qemu-system-riscv64` | `-machine virt -bios default` + pflash1 |
| aarch64 | `qemu-system-aarch64` | `-cpu max -machine virt,virtualization=on` |
| x86_64 | `qemu-system-x86_64` | `-machine q35 -cpu EPYC` |

## Key Dependencies

| Crate | Role |
|---|---|
| `axstd` | ArceOS standard library (`no_std` replacement) |
| `axhal` | Hardware abstraction layer (paging, traps) |
| `axmm` | Memory management (address spaces, page tables) |
| `axfs` | Filesystem access (FAT32 disk image) |
| `riscv` | RISC-V register access (riscv64 only) |
| `sbi-spec` / `sbi-rt` | SBI specification and runtime (riscv64 only) |

## License

GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0
