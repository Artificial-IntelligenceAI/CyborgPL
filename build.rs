// Compiles our custom 128-bit float software implementation
// (runtime/fp128/quadmath.c) into a static library, and exposes its path to
// main.rs via an env var baked in at compile time -- main.rs needs to pass
// this library to `cc` when linking a *user's* compiled .cyborgpl program
// (since that's where fp128 arithmetic actually gets used), not just to
// cyborgpl's own build.
fn main() {
    cc::Build::new()
        .file("runtime/fp128/quadmath.c")
        .opt_level(2)
        // Match the deployment target the final `cc` link step (in main.rs,
        // linking a user's compiled program) assumes by default -- without
        // this, the two disagree and `cc` prints a harmless but noisy
        // "built for newer macOS version" warning on every run.
        .flag("-mmacosx-version-min=26.0")
        .compile("cyborgpl_fp128");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let lib_path = format!("{out_dir}/libcyborgpl_fp128.a");
    println!("cargo:rustc-env=CYBORGPL_FP128_LIB={lib_path}");
    println!("cargo:rerun-if-changed=runtime/fp128/quadmath.c");
}
