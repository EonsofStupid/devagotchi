//! `bridge-mcp`: the adapter as its own executable. `devagotchi mcp` is the same code.

fn main() -> std::process::ExitCode {
    devagotchi_mcp::cli(std::env::args_os().skip(1).collect())
}
