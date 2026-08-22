fn main() {
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");

    if env_path.exists() {
        for item in dotenvy::from_path_iter(&env_path).unwrap() {
            let (key, value) = item.unwrap();
            println!("cargo:rustc-env={}={}", key, value);
        }
    }

    embuild::espidf::sysenv::output();
}
