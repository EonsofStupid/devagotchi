//! `devagotchi-hub`: the hub as its own executable. `devagotchi hub` is the same code.

fn main() -> std::process::ExitCode {
    devagotchi_hub::cli(std::env::args_os().skip(1).collect())
}
