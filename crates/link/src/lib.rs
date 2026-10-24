//! devagotchi-link: lets a Devagotchi hub reach the broker on this machine without
//! opening any port here. It dials out to the hub, proves this machine's key, and hands
//! each routed request to the local broker over private IPC. Jobs stay with the broker.

//! This library is the link's whole behaviour; `devagotchi-link` and `devagotchi link`
//! are two names for [`cli`].

mod session;
mod state;

use std::{env, ffi::OsString, path::PathBuf, process::ExitCode, time::Duration};

use devagotchi_contracts::{BridgeError, BridgeResult};

use crate::{
    session::{Ended, Mode},
    state::LinkConfig,
};

pub const USAGE: &str = "Usage:\n  devagotchi link enroll --state-dir ABS_PATH --hub URL --device NAME --broker ENDPOINT\n  devagotchi link run --state-dir ABS_PATH\n  devagotchi link --help\n\nenroll binds this machine's key to NAME on the hub with a one-time code from the hub\noperator (devagotchi hub enroll), then exits. The code is read from the\nDEVAGOTCHI_ENROLL_CODE environment variable, or else from the first line of stdin, never\nfrom the command line where other users could see it. run holds the link open and\nreconnects on its own; start it at login. URL is the hub's https public URL. ENDPOINT is\nthe local broker's ready line (Unix socket path or named pipe).";

/// Where `enroll` reads the one-time code from first.
const CODE_ENV: &str = "DEVAGOTCHI_ENROLL_CODE";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Enroll { state: PathBuf, config: LinkConfig },
    Run { state: PathBuf },
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> BridgeResult<Command> {
    let mut args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.as_slice() == ["--help"] {
        return Ok(Command::Help);
    }
    let invalid = || BridgeError::new("invalid_cli", "see --help for the accepted forms");
    if args.is_empty() {
        return Err(invalid());
    }
    let verb = args.remove(0);
    let mut flags = std::collections::BTreeMap::new();
    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        let value = iter.next().ok_or_else(invalid)?;
        if flags.insert(flag, value).is_some() {
            return Err(invalid());
        }
    }
    let mut take = |name: &str| flags.remove(name);
    let state = take("--state-dir").map(PathBuf::from).ok_or_else(invalid)?;
    if !state.is_absolute() {
        return Err(BridgeError::new(
            "invalid_cli",
            "--state-dir must be absolute",
        ));
    }
    let command = match verb.as_str() {
        "enroll" => {
            let config = LinkConfig {
                hub: take("--hub").ok_or_else(invalid)?,
                device: take("--device").ok_or_else(invalid)?,
                broker: take("--broker").ok_or_else(invalid)?,
            };
            config.validate()?;
            Command::Enroll { state, config }
        }
        "run" => Command::Run { state },
        _ => return Err(invalid()),
    };
    if !flags.is_empty() {
        return Err(invalid());
    }
    Ok(command)
}

/// The link's command line: `enroll …`, `run …`, or `--help`. Exit 2 means the hub
/// refused this device; a supervisor should not hammer it.
pub fn cli(args: Vec<OsString>) -> ExitCode {
    let command = match parse_args(args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(command) => command,
        Err(error) => {
            eprintln!("devagotchi-link: {error}\n\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("devagotchi-link: runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(dispatch(command))
}

async fn dispatch(command: Command) -> ExitCode {
    let result = match command {
        Command::Enroll { state, config } => match enrollment_code() {
            Ok(code) => enroll(state, config, code).await,
            Err(error) => Err(error),
        },
        Command::Run { state } => run(state).await,
        Command::Help => unreachable!(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("devagotchi-link: {error}");
            // 2 = the hub refused this device; a supervisor should not hammer it.
            ExitCode::from(2)
        }
    }
}

/// The one-time code: from the environment, else the first line of stdin.
fn enrollment_code() -> BridgeResult<String> {
    let code = match env::var(CODE_ENV) {
        Ok(code) => code,
        Err(_) => {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| BridgeError::new("invalid_cli", e.to_string()))?;
            line
        }
    };
    let code = code.trim().to_string();
    if code.is_empty() || code.len() > 128 {
        return Err(BridgeError::new(
            "invalid_cli",
            format!("no enrollment code: set {CODE_ENV} or pipe the code on stdin"),
        ));
    }
    Ok(code)
}

async fn enroll(state_dir: PathBuf, config: LinkConfig, code: String) -> BridgeResult<()> {
    state::prepare(&state_dir)?;
    let key = state::device_key(&state_dir)?;
    match session::connect(&config, &key, Mode::Enroll { code: &code }).await {
        Ended::Enrolled => {
            state::save_config(&state_dir, &config)?;
            eprintln!(
                "devagotchi-link enrolled: {} on {}. Start it with: devagotchi-link run --state-dir {}",
                config.device,
                config.hub,
                state_dir.display()
            );
            Ok(())
        }
        Ended::Refused(error) => Err(error),
        Ended::Retry(why) => Err(BridgeError::new(
            "enroll_failed",
            format!("{why}; the code is still unused if the hub never answered"),
        )),
    }
}

async fn run(state_dir: PathBuf) -> BridgeResult<()> {
    state::prepare(&state_dir)?;
    let config = state::load_config(&state_dir)?;
    let key = state::device_key(&state_dir)?;
    let mut delay = Duration::from_secs(1);
    loop {
        let started = tokio::time::Instant::now();
        let ended = tokio::select! {
            ended = session::connect(&config, &key, Mode::Serve) => ended,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("devagotchi-link stopped");
                return Ok(());
            }
        };
        match ended {
            Ended::Refused(error) => return Err(error),
            Ended::Enrolled => unreachable!("serve mode never enrolls"),
            Ended::Retry(why) => {
                // A link that held for a while starts its backoff over.
                if started.elapsed() > Duration::from_secs(60) {
                    delay = Duration::from_secs(1);
                }
                let pause = jitter(delay);
                eprintln!(
                    "devagotchi-link: {why}; reconnecting in {:.1}s",
                    pause.as_secs_f32()
                );
                tokio::select! {
                    _ = tokio::time::sleep(pause) => {}
                    _ = tokio::signal::ctrl_c() => {
                        eprintln!("devagotchi-link stopped");
                        return Ok(());
                    }
                }
                delay = (delay * 2).min(Duration::from_secs(60));
            }
        }
    }
}

/// Up to 25% random spread so a fleet that lost the hub together does not return together.
fn jitter(delay: Duration) -> Duration {
    let mut byte = [0u8; 1];
    let _ = getrandom::fill(&mut byte);
    delay + delay.mul_f32(f32::from(byte[0]) / 255.0 * 0.25)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const STATE: &str = "/s";
    #[cfg(windows)]
    const STATE: &str = r"C:\s";
    #[cfg(unix)]
    const BROKER: &str = "/run/b.sock";
    #[cfg(windows)]
    const BROKER: &str = r"\\.\pipe\devagotchi-broker-x";

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn enroll_needs_every_flag_and_valid_values() {
        let base = [
            "enroll",
            "--state-dir",
            STATE,
            "--hub",
            "https://hub.example",
            "--device",
            "gus",
            "--broker",
            BROKER,
        ];
        let ok = parse_args(args(&base)).unwrap();
        assert!(matches!(ok, Command::Enroll { ref config, .. } if config.device == "gus"));
        // The code is never a command-line argument, where other users could read it.
        let mut with_code = base.to_vec();
        with_code.extend(["--code", "dvg-abc"]);
        assert!(parse_args(args(&with_code)).is_err());
        assert!(parse_args(args(&base[..5])).is_err());
        let mut plain_http = base.to_vec();
        plain_http[4] = "http://hub.example";
        assert!(parse_args(args(&plain_http)).is_err());
    }

    #[test]
    fn run_takes_only_a_state_dir() {
        assert_eq!(
            parse_args(args(&["run", "--state-dir", STATE])).unwrap(),
            Command::Run {
                state: PathBuf::from(STATE)
            }
        );
        assert!(parse_args(args(&["run", "--state-dir", "relative"])).is_err());
        assert!(parse_args(args(&["run", "--state-dir", STATE, "--hub", "x"])).is_err());
        assert_eq!(parse_args(args(&["--help"])).unwrap(), Command::Help);
    }

    #[test]
    fn jitter_stays_within_a_quarter() {
        let base = Duration::from_secs(8);
        for _ in 0..50 {
            let pause = jitter(base);
            assert!(pause >= base && pause <= base.mul_f32(1.25));
        }
    }
}
