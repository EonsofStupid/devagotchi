//! `devagotchi-link`: the link as its own executable. `devagotchi link` is the same code.

fn main() -> std::process::ExitCode {
    devagotchi_link::cli(std::env::args_os().skip(1).collect())
}
