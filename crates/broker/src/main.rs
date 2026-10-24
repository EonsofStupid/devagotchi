//! `bridge-broker`: the broker as its own executable. `devagotchi broker run` is the same code.

fn main() -> std::process::ExitCode {
    devagotchi_broker::cli(std::env::args_os().skip(1).collect())
}
