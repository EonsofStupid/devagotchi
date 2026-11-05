//! Which Windows command lines run in Git Bash rather than PowerShell, and where Git for
//! Windows' bash.exe is. Pure string logic over an environment lookup and a file check, so
//! it builds and is tested on every OS; only the Windows provider calls it.
#![cfg_attr(not(windows), allow(dead_code))]

/// What `run` says when a line needs Git Bash and the machine has none.
pub(crate) const NO_GIT_BASH: &str = "This command runs a shell script, and this Windows machine has no Git Bash. Install Git for Windows (winget install --id Git.Git -e) and run it again.";

/// The first token of the trimmed line, with a leading quoted token unquoted, and the
/// rest of the line after it.
fn first_token(line: &str) -> (&str, bool, &str) {
    let line = line.trim();
    if let Some(quote @ ('"' | '\'')) = line.chars().next() {
        let body = &line[1..];
        return match body.find(quote) {
            Some(end) => (&body[..end], true, &body[end + 1..]),
            None => (body, true, ""),
        };
    }
    let end = line.find(char::is_whitespace).unwrap_or(line.len());
    (&line[..end], false, &line[end..])
}

/// True when the line runs a shell script: its first token ends in `.sh`, or is `bash`,
/// `sh`, `bash.exe` or `sh.exe` (ASCII case-insensitive, as Windows file names are).
pub(crate) fn runs_shell_script(line: &str) -> bool {
    let (token, _, _) = first_token(line);
    script_path(token) || is_bash(token)
}

fn script_path(token: &str) -> bool {
    token.len() > 3 && token[token.len() - 3..].eq_ignore_ascii_case(".sh")
}

fn is_bash(token: &str) -> bool {
    ["bash", "sh", "bash.exe", "sh.exe"]
        .iter()
        .any(|name| token.eq_ignore_ascii_case(name))
}

/// The `-c` text for a line that runs a shell script, or `None` for any other line.
/// A line that starts with bash or sh is given as written. A line that starts with a
/// `.sh` path runs that script with this same bash (`"$BASH" <path> <rest>`): Git Bash
/// treats a file as executable only when it starts with `#!`, so a script without one
/// would otherwise be refused. An unquoted path holding backslashes is single-quoted, so
/// `C:\dir\a.sh` keeps them; the rest of the line is unchanged.
pub(crate) fn bash_line(line: &str) -> Option<String> {
    let (token, quoted, rest) = first_token(line);
    if is_bash(token) {
        return Some(line.trim().to_owned());
    }
    if !script_path(token) {
        return None;
    }
    let trimmed = line.trim();
    let written = &trimmed[..trimmed.len() - rest.len()];
    let word = if !quoted && token.contains('\\') && !token.contains(['\'', '$']) {
        format!("'{token}'")
    } else {
        written.to_owned()
    };
    Some(format!("\"$BASH\" {word}{rest}"))
}

/// Git for Windows' bash.exe, first hit wins: `DEVAGOTCHI_BASH`, then Git under
/// `%ProgramFiles%`, `%ProgramFiles(x86)%` and `%LOCALAPPDATA%\Programs`, then beside the
/// `git.exe` on `PATH` (`Git\cmd` or `Git\bin`). `is_file` says whether a path is a file.
/// A path under System32, SysWOW64 or Sysnative is never chosen: that bash.exe is WSL.
pub(crate) fn find_bash(
    env: impl Fn(&str) -> Option<String>,
    is_file: impl Fn(&str) -> bool,
) -> Option<String> {
    let usable = |path: &str| !is_system_dir(path) && is_file(path);
    if let Some(path) = env("DEVAGOTCHI_BASH").filter(|path| usable(path)) {
        return Some(path);
    }
    let installed = [
        ("ProgramFiles", r"Git\bin\bash.exe"),
        ("ProgramFiles(x86)", r"Git\bin\bash.exe"),
        ("LOCALAPPDATA", r"Programs\Git\bin\bash.exe"),
    ];
    for (var, tail) in installed {
        if let Some(base) = env(var).filter(|base| !base.trim().is_empty()) {
            let path = join(&base, tail);
            if usable(&path) {
                return Some(path);
            }
        }
    }
    let path_var = env("PATH").or_else(|| env("Path")).unwrap_or_default();
    for dir in path_var.split(';').map(|dir| dir.trim().trim_matches('"')) {
        if dir.is_empty() || !is_file(&join(dir, "git.exe")) {
            continue;
        }
        let dir = dir.trim_end_matches(['\\', '/']);
        let beside = parent(dir).map(|git| join(git, r"bin\bash.exe"));
        for path in beside.into_iter().chain([join(dir, "bash.exe")]) {
            if usable(&path) {
                return Some(path);
            }
        }
    }
    None
}

fn join(base: &str, tail: &str) -> String {
    format!("{}\\{tail}", base.trim_end_matches(['\\', '/']))
}

fn parent(dir: &str) -> Option<&str> {
    dir.rfind(['\\', '/']).map(|at| &dir[..at]).filter(|parent| !parent.is_empty())
}

fn is_system_dir(path: &str) -> bool {
    path.split(['\\', '/']).any(|part| {
        ["system32", "syswow64", "sysnative"]
            .iter()
            .any(|name| part.eq_ignore_ascii_case(name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn a_script_is_a_sh_path_or_bash_first() {
        assert!(runs_shell_script("./scripts/check.sh"));
        assert!(runs_shell_script(r#""C:\x y\a.sh" --flag"#));
        assert!(runs_shell_script("bash -c 'echo hi'"));
        assert!(runs_shell_script("  SH.EXE -c true"));
        assert!(runs_shell_script(r"C:\repo\BUILD.SH"));
        assert!(!runs_shell_script("Get-ChildItem"));
        assert!(!runs_shell_script("notes.sh.txt"));
        assert!(!runs_shell_script("bashful"));
        assert!(!runs_shell_script(".sh"));
        assert!(!runs_shell_script("Write-Output a.sh"));
    }

    #[test]
    fn the_bash_line_keeps_what_was_written() {
        assert_eq!(bash_line("Get-ChildItem"), None);
        assert_eq!(bash_line("bash -c 'echo hi'").as_deref(), Some("bash -c 'echo hi'"));
        assert_eq!(
            bash_line("./scripts/check.sh --all").as_deref(),
            Some(r#""$BASH" ./scripts/check.sh --all"#)
        );
        assert_eq!(
            bash_line(r#" "C:\x y\a.sh" --flag"#).as_deref(),
            Some(r#""$BASH" "C:\x y\a.sh" --flag"#)
        );
        assert_eq!(
            bash_line(r"C:\tmp\ok.sh a\b").as_deref(),
            Some(r#""$BASH" 'C:\tmp\ok.sh' a\b"#)
        );
    }

    fn find(env: &[(&str, &str)], files: &[&str]) -> Option<String> {
        let env: HashMap<String, String> =
            env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        find_bash(|name| env.get(name).cloned(), |path| files.contains(&path))
    }

    const PF: &str = r"C:\Program Files\Git\bin\bash.exe";
    const PF86: &str = r"C:\Program Files (x86)\Git\bin\bash.exe";
    const LOCAL: &str = r"C:\Users\j\AppData\Local\Programs\Git\bin\bash.exe";
    const WSL: &str = r"C:\Windows\System32\bash.exe";

    fn all_env(path: &str) -> Vec<(&'static str, String)> {
        vec![
            ("ProgramFiles", r"C:\Program Files".into()),
            ("ProgramFiles(x86)", r"C:\Program Files (x86)".into()),
            ("LOCALAPPDATA", r"C:\Users\j\AppData\Local".into()),
            ("PATH", path.into()),
        ]
    }

    fn find_all(extra: &[(&str, &str)], path: &str, files: &[&str]) -> Option<String> {
        let owned = all_env(path);
        let mut env: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        env.extend_from_slice(extra);
        find(&env, files)
    }

    #[test]
    fn the_lookup_takes_the_first_hit_in_order() {
        let every = [r"D:\tools\bash.exe", PF, PF86, LOCAL];
        let pick = |extra: &[(&str, &str)], files: &[&str]| find_all(extra, "", files);
        let env = [("DEVAGOTCHI_BASH", r"D:\tools\bash.exe")];
        assert_eq!(pick(&env, &every).as_deref(), Some(r"D:\tools\bash.exe"));
        // DEVAGOTCHI_BASH that is not a file is passed over.
        assert_eq!(pick(&env, &every[1..]).as_deref(), Some(PF));
        assert_eq!(pick(&[], &every[2..]).as_deref(), Some(PF86));
        assert_eq!(pick(&[], &every[3..]).as_deref(), Some(LOCAL));
        assert_eq!(pick(&[], &[]), None);
    }

    #[test]
    fn git_on_path_finds_its_bash() {
        let cmd = r"E:\Git\cmd\git.exe";
        let bash = r"E:\Git\bin\bash.exe";
        let path = r"C:\Windows\System32;E:\Git\cmd\";
        assert_eq!(find_all(&[], path, &[cmd, bash]).as_deref(), Some(bash));
        let path = r"E:\Git\bin";
        assert_eq!(
            find_all(&[], path, &[r"E:\Git\bin\git.exe", bash]).as_deref(),
            Some(bash)
        );
        // An installed Git still wins over PATH.
        assert_eq!(find_all(&[], r"E:\Git\cmd", &[cmd, bash, PF]).as_deref(), Some(PF));
        // git.exe on PATH without a bash beside it finds nothing.
        assert_eq!(find_all(&[], r"E:\Git\cmd", &[cmd]), None);
    }

    #[test]
    fn system32_bash_is_never_chosen() {
        let env = [("DEVAGOTCHI_BASH", WSL)];
        assert_eq!(find_all(&env, "", &[WSL]), None);
        let env = [("DEVAGOTCHI_BASH", r"C:\WINDOWS\SysNative\bash.exe")];
        assert_eq!(find_all(&env, "", &[r"C:\WINDOWS\SysNative\bash.exe"]), None);
        // git.exe in System32 would put bash.exe beside it there: refused.
        let path = r"C:\Windows\System32";
        let files = [r"C:\Windows\System32\git.exe", WSL, r"C:\Windows\bin\bash.exe"];
        assert_eq!(find_all(&[], path, &files).as_deref(), Some(r"C:\Windows\bin\bash.exe"));
        assert_eq!(find_all(&[], path, &files[..2]), None);
        assert_eq!(find_all(&env, path, &[WSL]), None);
    }
}
