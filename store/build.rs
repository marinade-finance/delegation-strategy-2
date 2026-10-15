fn main() {
    // include_dir does not track new files in migrations/ on stable Rust.
    println!("cargo:rerun-if-changed=../migrations");
}
