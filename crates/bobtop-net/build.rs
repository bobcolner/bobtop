//! Build script.
//!
//! When the `ebpf` feature is enabled on Linux, compiles `bpf/bobtop_net.bpf.c`
//! to a BPF object via `clang -target bpf` and places the result in
//! `$OUT_DIR/bobtop_net.bpf.o`. The userspace loader picks it up via
//! `include_bytes!`.
//!
//! When the feature is off (or we're not on Linux) this is a no-op and the
//! loader falls back to its compiled-in empty object.

fn main() {
    println!("cargo:rerun-if-changed=bpf/bobtop_net.bpf.c");
    // Always declare the cfg so rustc doesn't warn even when the feature is off.
    println!("cargo:rustc-check-cfg=cfg(bobtop_bpf_built)");

    #[cfg(all(target_os = "linux", feature = "ebpf"))]
    compile_bpf();
}

#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn compile_bpf() {
    use std::env;
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("bpf/bobtop_net.bpf.c");
    let obj = out_dir.join("bobtop_net.bpf.o");

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
            "-c",
        ])
        .arg("-I")
        .arg(arch_inc)
        .arg(src)
        .arg("-o")
        .arg(&obj)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-env=BOBTOP_BPF_OBJ={}", obj.display());
            // Convey build success to the loader via a cfg, so we don't need
            // to runtime-check `BPF_OBJECT.is_empty()`.
            println!("cargo:rustc-cfg=bobtop_bpf_built");
        }
        Ok(s) => {
            println!(
                "cargo:warning=clang exit {} compiling bobtop_net.bpf.c — eBPF tier disabled",
                s
            );
        }
        Err(e) => {
            println!(
                "cargo:warning=clang invocation failed ({e}) — eBPF tier disabled. \
                 Install clang + libbpf-dev to enable Tier 3 attribution."
            );
        }
    }
}
