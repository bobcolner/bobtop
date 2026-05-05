//! Build script.
//!
//! When the `ebpf` feature is enabled on Linux, compiles two BPF source
//! files via `clang -target bpf`:
//!
//! - `bpf/bobtop_net.bpf.c`  — Tier 3 network attribution (TCP kprobes)
//! - `bpf/bobtop_disk.bpf.c` — Tier 2 disk attribution (vfs_{read,write})
//!
//! Each is placed at `$OUT_DIR/<name>.bpf.o` and exposed via a per-object
//! `cargo:rustc-env=` so the userspace loader can `include_bytes!` it. The
//! `bobtop_bpf_built` cfg is set when *both* compile successfully — partial
//! success would leave the userspace code with one tier loaded and one
//! silently empty, which is harder to diagnose than a clean off-state.
//!
//! When the feature is off (or we're not on Linux) this is a no-op and
//! the loader falls back to its compiled-in empty objects.

fn main() {
    println!("cargo:rerun-if-changed=bpf/bobtop_net.bpf.c");
    println!("cargo:rerun-if-changed=bpf/bobtop_disk.bpf.c");
    // Always declare the cfg so rustc doesn't warn even when the feature is off.
    println!("cargo:rustc-check-cfg=cfg(bobtop_bpf_built)");

    #[cfg(all(target_os = "linux", feature = "ebpf"))]
    compile_bpf();
}

#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn compile_bpf() {
    use std::env;
    use std::path::PathBuf;

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    let net_ok = compile_one(
        &manifest_dir.join("bpf/bobtop_net.bpf.c"),
        &out_dir.join("bobtop_net.bpf.o"),
        "BOBTOP_BPF_OBJ",
    );
    let disk_ok = compile_one(
        &manifest_dir.join("bpf/bobtop_disk.bpf.c"),
        &out_dir.join("bobtop_disk.bpf.o"),
        "BOBTOP_BPF_DISK_OBJ",
    );

    if net_ok && disk_ok {
        println!("cargo:rustc-cfg=bobtop_bpf_built");
    }
}

#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn compile_one(src: &std::path::Path, obj: &std::path::Path, env_var: &str) -> bool {
    use std::process::Command;

    // Best-effort include search for vmlinux / asm/types.h. clang -target bpf
    // doesn't get the host's asm/ headers automatically; mirror the typical
    // libbpf-bootstrap invocation.
    let arch_inc = "/usr/include/x86_64-linux-gnu";

    let status = Command::new("clang")
        .args([
            "-O2",
            "-g",
            "-Wall",
            "-Werror",
            "-target",
            "bpf",
            "-D__TARGET_ARCH_x86",
            "-mcpu=v3",
            // Suppress the `.llvm_addrsig` section. aya 0.13's ELF parser
            // is known to reject objects that contain LLVM-specific
            // section types it doesn't recognize, returning the cryptic
            // "error parsing ELF data". Newer clangs emit this section by
            // default for link-time-optimization purposes; BPF objects
            // don't go through a linker so we never need it.
            "-fno-addrsig",
            "-c",
        ])
        .arg("-I")
        .arg(arch_inc)
        .arg(src)
        .arg("-o")
        .arg(obj)
        .status();

    match status {
        Ok(s) if s.success() => {
            // Strip DWARF debug sections — keep `.BTF` / `.BTF.ext` because
            // aya needs them, but drop `.debug_*` (DWARF) which aya 0.13's
            // ELF parser rejects with "error parsing ELF data". `llvm-strip`
            // ships with clang.
            let strip_status = Command::new("llvm-strip")
                .args(["--strip-debug"])
                .arg(obj)
                .status()
                .or_else(|_| {
                    Command::new("llvm-strip-19")
                        .args(["--strip-debug"])
                        .arg(obj)
                        .status()
                });
            if let Err(e) = strip_status {
                println!(
                    "cargo:warning=llvm-strip not available ({e}); aya may fail to load — install LLVM"
                );
            }
            println!("cargo:rustc-env={}={}", env_var, obj.display());
            true
        }
        Ok(s) => {
            println!(
                "cargo:warning=clang exit {} compiling {} — eBPF tier disabled",
                s,
                src.display()
            );
            false
        }
        Err(e) => {
            println!(
                "cargo:warning=clang invocation failed ({e}) compiling {} — eBPF tier disabled. \
                 Install clang + libbpf-dev to enable Tier 3 attribution.",
                src.display()
            );
            false
        }
    }
}
