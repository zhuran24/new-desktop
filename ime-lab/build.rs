fn main() {
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let variant = dir.file_name().unwrap().to_str().unwrap();
    let commit = match variant {
        "v070" => "0c830f4d257e69fdd17200650533ab4ca9a40cc0",
        "main" => "4c7f1350331562436df868c55ac33bebc4c6406c",
        _ => panic!("unknown variant"),
    };
    println!("cargo:rustc-env=IME_LAB_VARIANT={variant}");
    println!("cargo:rustc-env=IME_LAB_COMMIT={commit}");
    println!(
        "cargo:rustc-env=IME_LAB_ROOT={}",
        dir.parent().unwrap().parent().unwrap().display()
    );
    println!("cargo:rerun-if-changed=../../build.rs");
}
