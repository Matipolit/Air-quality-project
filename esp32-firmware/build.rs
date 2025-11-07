fn main() {
    if std::path::Path::new(".env").exists() {
        for item in dotenvy::dotenv_iter().unwrap() {
            let (key, value) = item.unwrap();
            println!("cargo:rustc-env={}={}", key, value);
        }
    }
    embuild::espidf::sysenv::output();
}
