use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

/// ArceOS Guest Address Space — multi-architecture build & run tool
#[derive(Parser)]
#[command(name = "xtask", about = "Build and run arceos-guestaspace on different architectures")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the kernel for a given architecture
    Build {
        /// Target architecture: riscv64, aarch64, x86_64
        #[arg(long, default_value = "riscv64")]
        arch: String,
    },
    /// Build and run the kernel in QEMU
    Run {
        /// Target architecture: riscv64, aarch64, x86_64
        #[arg(long, default_value = "riscv64")]
        arch: String,
    },
}

#[derive(Clone)]
struct ArchInfo {
    target: &'static str,
    platform: &'static str,
    objcopy_arch: &'static str,
}

fn arch_info(arch: &str) -> ArchInfo {
    match arch {
        "riscv64" => ArchInfo {
            target: "riscv64gc-unknown-none-elf",
            platform: "riscv64-qemu-virt",
            objcopy_arch: "riscv64",
        },
        "aarch64" => ArchInfo {
            target: "aarch64-unknown-none-softfloat",
            platform: "aarch64-qemu-virt",
            objcopy_arch: "aarch64",
        },
        "x86_64" => ArchInfo {
            target: "x86_64-unknown-none",
            platform: "x86-pc",
            objcopy_arch: "x86_64",
        },
        _ => {
            eprintln!(
                "Error: unsupported architecture '{}'. \
                 Supported: riscv64, aarch64, x86_64",
                arch
            );
            process::exit(1);
        }
    }
}

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn install_config(root: &Path, arch: &str) {
    let src = root.join("configs").join(format!("{arch}.toml"));
    let dst = root.join(".axconfig.toml");
    if !src.exists() {
        eprintln!("Error: config file not found: {}", src.display());
        process::exit(1);
    }
    std::fs::copy(&src, &dst).unwrap_or_else(|e| {
        eprintln!("Error: failed to copy config: {}", e);
        process::exit(1);
    });
    println!("Installed config: {} -> .axconfig.toml", src.display());
}

/// Build the guest payload (gkernel) for the target architecture.
fn build_payload(root: &Path, info: &ArchInfo) -> PathBuf {
    let payload_dir = root.join("payload").join("gkernel");
    let manifest = payload_dir.join("Cargo.toml");

    println!("Building payload (gkernel) ...");

    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--manifest-path",
            manifest.to_str().unwrap(),
            "--target",
            info.target,
        ])
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Error: failed to run cargo build for payload: {}", e);
            process::exit(1);
        });

    if !status.success() {
        eprintln!("Error: payload compilation failed");
        process::exit(status.code().unwrap_or(1));
    }

    let payload_elf = payload_dir
        .join("target")
        .join(info.target)
        .join("release")
        .join("gkernel");

    let payload_bin = payload_elf.with_extension("bin");

    let status = Command::new("rust-objcopy")
        .args([
            &format!("--binary-architecture={}", info.objcopy_arch),
            "--only-section=.text",
            payload_elf.to_str().unwrap(),
            "--strip-all",
            "-O",
            "binary",
            payload_bin.to_str().unwrap(),
        ])
        .status()
        .expect("failed to execute rust-objcopy for payload");

    if !status.success() {
        eprintln!("Error: rust-objcopy for payload failed");
        process::exit(status.code().unwrap_or(1));
    }

    println!("Payload built: {}", payload_bin.display());
    payload_bin
}

/// Create a 64MB FAT32 disk image containing `/sbin/gkernel`.
fn create_fat_disk_image(path: &Path, payload_bin: &Path) {
    const DISK_SIZE: u64 = 64 * 1024 * 1024;

    let payload_data = std::fs::read(payload_bin).unwrap_or_else(|e| {
        eprintln!(
            "Error: failed to read payload {}: {}",
            payload_bin.display(),
            e
        );
        process::exit(1);
    });
    println!("Payload binary size: {} bytes", payload_data.len());

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap_or_else(|e| {
            eprintln!("Error: failed to create disk image: {}", e);
            process::exit(1);
        });
    file.set_len(DISK_SIZE).unwrap();

    let format_opts = fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32);
    fatfs::format_volume(&file, format_opts).unwrap_or_else(|e| {
        eprintln!("Error: failed to format FAT32: {}", e);
        process::exit(1);
    });

    {
        let fs = fatfs::FileSystem::new(&file, fatfs::FsOptions::new()).unwrap_or_else(|e| {
            eprintln!("Error: failed to open FAT filesystem: {}", e);
            process::exit(1);
        });
        let root_dir = fs.root_dir();

        root_dir.create_dir("sbin").unwrap_or_else(|e| {
            eprintln!("Error: failed to create /sbin: {}", e);
            process::exit(1);
        });

        let mut f = root_dir.create_file("sbin/gkernel").unwrap_or_else(|e| {
            eprintln!("Error: failed to create /sbin/gkernel: {}", e);
            process::exit(1);
        });
        f.write_all(&payload_data).unwrap();
        f.flush().unwrap();
    }

    println!(
        "Created FAT32 disk image: {} ({}MB) with /sbin/gkernel",
        path.display(),
        DISK_SIZE / (1024 * 1024)
    );
}

/// Create a pflash image with magic "pfld" at offset 0 (for riscv64 NPF test).
fn create_pflash_image(root: &Path, arch: &str) -> PathBuf {
    let size: usize = match arch {
        "riscv64" => 32 * 1024 * 1024,     // 32MB - QEMU virt pflash1
        "aarch64" => 64 * 1024 * 1024,     // 64MB - QEMU virt pflash1
        _ => 4 * 1024 * 1024,
    };

    let pflash_path = root.join("target").join(format!("pflash-{arch}.img"));
    let mut image = vec![0xFFu8; size];

    // Write magic "pfld" at offset 0 (consistent with h_2_0 format)
    image[0..4].copy_from_slice(b"pfld");

    std::fs::write(&pflash_path, &image).unwrap_or_else(|e| {
        eprintln!("Error: failed to write pflash image: {}", e);
        process::exit(1);
    });
    println!(
        "Created pflash image: {} ({} bytes)",
        pflash_path.display(),
        size
    );
    pflash_path
}

/// Build the hypervisor kernel.
fn do_build(root: &Path, info: &ArchInfo) {
    let manifest = root.join("Cargo.toml");
    let axconfig_path = root.join(".axconfig.toml");
    let status = Command::new("cargo")
        .env("AX_CONFIG_PATH", axconfig_path.to_str().unwrap())
        .args([
            "build",
            "--release",
            "--target",
            info.target,
            "--features",
            "axstd",
            "--manifest-path",
            manifest.to_str().unwrap(),
        ])
        .status()
        .expect("failed to execute cargo build");
    if !status.success() {
        eprintln!("Error: cargo build failed");
        process::exit(status.code().unwrap_or(1));
    }
}

/// Convert ELF to raw binary.
fn do_objcopy(elf: &Path, bin: &Path, objcopy_arch: &str) {
    let status = Command::new("rust-objcopy")
        .args([
            &format!("--binary-architecture={objcopy_arch}"),
            elf.to_str().unwrap(),
            "--strip-all",
            "-O",
            "binary",
            bin.to_str().unwrap(),
        ])
        .status()
        .expect("failed to execute rust-objcopy");
    if !status.success() {
        eprintln!("Error: rust-objcopy failed");
        process::exit(status.code().unwrap_or(1));
    }
}

/// Run QEMU with VirtIO block device and optional pflash.
fn do_run_qemu(arch: &str, elf: &Path, bin: &Path, disk: &Path, pflash: Option<&Path>) {
    let mem = "128M";
    let smp = "1";
    let qemu = format!("qemu-system-{arch}");

    let mut args: Vec<String> = vec![
        "-m".into(),
        mem.into(),
        "-smp".into(),
        smp.into(),
        "-nographic".into(),
    ];

    match arch {
        "riscv64" => {
            args.extend([
                "-machine".into(),
                "virt".into(),
                "-bios".into(),
                "default".into(),
                "-kernel".into(),
                bin.to_str().unwrap().into(),
            ]);
            // Attach pflash1 for pflash NPF test
            if let Some(pf) = pflash {
                args.extend([
                    "-drive".into(),
                    format!(
                        "if=pflash,format=raw,unit=1,file={},readonly=on",
                        pf.display()
                    ),
                ]);
            }
        }
        "aarch64" => {
            args.extend([
                "-cpu".into(),
                "max".into(),
                "-machine".into(),
                "virt,virtualization=on".into(),
                "-kernel".into(),
                bin.to_str().unwrap().into(),
            ]);
        }
        "x86_64" => {
            args.extend([
                "-machine".into(),
                "q35".into(),
                "-cpu".into(),
                "EPYC".into(),
                "-kernel".into(),
                elf.to_str().unwrap().into(),
            ]);
        }
        _ => unreachable!(),
    }

    // VirtIO block device (for disk image containing guest payload)
    args.extend([
        "-drive".into(),
        format!("file={},format=raw,if=none,id=disk0", disk.display()),
        "-device".into(),
        "virtio-blk-pci,drive=disk0".into(),
    ]);

    println!("Running: {} {}", qemu, args.join(" "));
    let status = Command::new(&qemu)
        .args(&args)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Error: failed to run {}: {}", qemu, e);
            process::exit(1);
        });
    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
}

fn main() {
    let cli = Cli::parse();
    let root = project_root();

    match cli.command {
        Cmd::Build { ref arch } => {
            let info = arch_info(arch);
            install_config(&root, arch);
            let _payload = build_payload(&root, &info);
            do_build(&root, &info);
            println!("Build complete for {arch} ({})", info.target);
        }
        Cmd::Run { ref arch } => {
            let info = arch_info(arch);
            install_config(&root, arch);

            // 1. Build payload (gkernel)
            let payload_bin = build_payload(&root, &info);

            // 2. Create disk image with payload
            let disk = root.join("target").join(format!("disk-{arch}.img"));
            create_fat_disk_image(&disk, &payload_bin);

            // 3. Create pflash image (for riscv64 NPF passthrough test)
            let pflash = if arch == "riscv64" {
                Some(create_pflash_image(&root, arch))
            } else {
                None
            };

            // 4. Build hypervisor kernel
            do_build(&root, &info);

            let elf = root
                .join("target")
                .join(info.target)
                .join("release")
                .join("arceos-guestaspace");
            let bin = elf.with_extension("bin");

            if arch != "x86_64" {
                do_objcopy(&elf, &bin, info.objcopy_arch);
            }

            // 5. Run QEMU
            do_run_qemu(arch, &elf, &bin, &disk, pflash.as_deref());
        }
    }
}
