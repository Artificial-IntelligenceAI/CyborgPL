// Compiles our own small C runtime pieces (a 128-bit software float
// implementation, and a GMP shim for `bignum`) into static libraries, and
// exposes their paths to main.rs via env vars baked in at compile time --
// main.rs needs to pass these to `cc` when linking a *user's* compiled
// .cyborgpl program (since that's where they actually get used), not just
// to cyborgpl's own build.
fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();

    cc::Build::new()
        .file("runtime/fp128/quadmath.c")
        .opt_level(2)
        // Match the deployment target the final `cc` link step (in main.rs,
        // linking a user's compiled program) assumes by default -- without
        // this, the two disagree and `cc` prints a harmless but noisy
        // "built for newer macOS version" warning on every run.
        .flag("-mmacosx-version-min=26.0")
        .compile("cyborgpl_fp128");
    println!("cargo:rustc-env=CYBORGPL_FP128_LIB={out_dir}/libcyborgpl_fp128.a");
    println!("cargo:rerun-if-changed=runtime/fp128/quadmath.c");

    // GMP: ask Homebrew where it actually is rather than hardcoding
    // /opt/homebrew (Apple Silicon) vs /usr/local (Intel).
    let gmp_prefix = std::process::Command::new("brew")
        .args(["--prefix", "gmp"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "/opt/homebrew/opt/gmp".to_string());

    cc::Build::new()
        .file("runtime/gmp/bignum_shim.c")
        .include(format!("{gmp_prefix}/include"))
        .opt_level(2)
        .flag("-mmacosx-version-min=26.0")
        .compile("cyborgpl_bignum");
    println!("cargo:rustc-env=CYBORGPL_BIGNUM_LIB={out_dir}/libcyborgpl_bignum.a");
    println!("cargo:rustc-env=CYBORGPL_GMP_LIB_DIR={gmp_prefix}/lib");
    println!("cargo:rerun-if-changed=runtime/gmp/bignum_shim.c");

    cc::Build::new()
        .file("runtime/io/input_shim.c")
        .opt_level(2)
        .flag("-mmacosx-version-min=26.0")
        .compile("cyborgpl_io");
    println!("cargo:rustc-env=CYBORGPL_IO_LIB={out_dir}/libcyborgpl_io.a");
    println!("cargo:rerun-if-changed=runtime/io/input_shim.c");
}
