//! The build's name, fixed at compile time: `DEVAGOTCHI_BUILD` from the environment (CI sets
//! it to the same string the hub's downloads `VERSION` holds: `main-<sha7>`, `ci-<sha7>` or a
//! release's version), else `dev`. `devagotchi --version` prints it.

fn main() {
    println!("cargo:rerun-if-env-changed=DEVAGOTCHI_BUILD");
    let build = std::env::var("DEVAGOTCHI_BUILD")
        .ok()
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| "dev".to_owned());
    println!("cargo:rustc-env=DEVAGOTCHI_BUILD={}", build.trim());
}
