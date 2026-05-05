//! Build script.
//!
//! When the `ebpf` feature is enabled on Linux, uses libbpf-cargo's
//! `SkeletonBuilder` to compile each BPF source to an ELF object AND
//! generate a typed Rust skeleton with named accessors for each program
//! and map. The skeletons are emitted to `$OUT_DIR/{net,disk}_skel.rs`
//! and consumed via `include!`.
//!
//! Migrated from a hand-rolled clang invocation + `aya` userspace loader
//! after persistent ELF-parse errors on hosts where the BPF object was
//! perfectly valid (the bug was aya 0.13's from-scratch ELF parser, not
//! the object). libbpf — the canonical BPF userspace library maintained
//! alongside the kernel — handles every format corner correctly.
//!
//! When the feature is off (or we're not on Linux) this is a no-op and
//! the loader falls back to its compiled-in empty skeletons.

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

    let net_ok = build_skel(
        &manifest_dir.join("bpf/bobtop_net.bpf.c"),
        &out_dir.join("net.skel.rs"),
    );
    let disk_ok = build_skel(
        &manifest_dir.join("bpf/bobtop_disk.bpf.c"),
        &out_dir.join("disk.skel.rs"),
    );

    if net_ok && disk_ok {
        println!("cargo:rustc-cfg=bobtop_bpf_built");
    }
}

#[cfg(all(target_os = "linux", feature = "ebpf"))]
fn build_skel(src: &std::path::Path, skel_out: &std::path::Path) -> bool {
    use libbpf_cargo::SkeletonBuilder;

    match SkeletonBuilder::new()
        .source(src)
        .clang_args([
            "-Wall",
            "-Werror",
            "-mcpu=v3",
            // libbpf-cargo passes -O2 + -g + -target bpf by default. It
            // also extracts libbpf headers (bpf/bpf_helpers.h, etc.) but
            // doesn't add the system /usr/include path needed for the
            // <linux/bpf.h> / <linux/ptrace.h> headers our sources use.
            // The path below matches the existing convention from our
            // pre-libbpf-cargo build script and works on Debian/Ubuntu.
            "-I",
            "/usr/include/x86_64-linux-gnu",
        ])
        .build_and_generate(skel_out)
    {
        Ok(()) => true,
        Err(e) => {
            // Print the full anyhow chain so the user can see clang's
            // actual stderr (e.g. "fatal error: 'asm/types.h' not found").
            println!(
                "cargo:warning=libbpf-cargo failed for {} — eBPF tier disabled:",
                src.display()
            );
            for cause in e.chain() {
                for line in cause.to_string().lines() {
                    println!("cargo:warning=    {line}");
                }
            }
            false
        }
    }
}
