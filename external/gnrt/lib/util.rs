// Copyright (c) 2026 vivo Mobile Communication Co., Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//       http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Process and path helpers for GN collection and Cargo resolution.

use anyhow::{bail, ensure, Context, Result};
use std::io::Write;
use std::{
    fs,
    path::{Path, PathBuf},
    process,
};

/// Resolves the compiler independently of vendored crates' rustup configuration.
///
/// The query supplies its own target; other compiler arguments are preserved.
///
/// # Errors
///
/// Returns an error for malformed commands, missing compilers, or failed
/// rustup lookups.
pub fn rustc_command(compiler: &str) -> Result<process::Command> {
    let words = shlex::split(compiler).context("invalid Rust compiler command quoting")?;
    let program = words.first().context("Rust compiler command is empty")?;
    let mut executable = match tool_path(program) {
        Ok(path) => path,
        // Toolchains install cargo and rustc into the same bin directory, so a
        // cargo located via override covers a rustc missing from PATH.
        Err(error) if program == "rustc" => rustc_next_to_cargo().ok_or(error)?,
        Err(error) => return Err(error),
    }
    .canonicalize()?;
    let mut args = Vec::new();
    let mut words = words.iter().skip(1);
    while let Some(arg) = words.next() {
        // The query supplies the target; keep flags that affect its cfg values.
        if arg == "--target" {
            let target = words.next().context("--target requires a value")?;
            ensure!(
                !target.is_empty() && !target.starts_with('-'),
                "--target requires a value"
            );
        } else if let Some(target) = arg.strip_prefix("--target=") {
            ensure!(!target.is_empty(), "--target requires a value");
        } else {
            args.push(arg.clone());
        }
    }
    if executable.file_name().and_then(|name| name.to_str()) == Some("rustup") {
        let toolchain = strip_rustup_routing(&mut args);
        let mut rustup = process::Command::new(&executable);
        if let Some(toolchain) = toolchain {
            rustup.args(["--toolchain", &toolchain]);
        }
        let output = rustup.args(["which", "rustc"]).output()?;
        check_exit_ok(&output, "resolving rustc through rustup")?;
        let path = String::from_utf8(output.stdout).context("rustup returned a non-UTF-8 path")?;
        executable = std::fs::canonicalize(path.trim())?;
    }
    let mut command = process::Command::new(executable);
    command.args(args);
    Ok(command)
}

/// Returns rustc installed beside cargo, or `None` when cargo cannot be located.
fn rustc_next_to_cargo() -> Option<PathBuf> {
    let cargo = tool_path("cargo").ok()?;
    let rustc = cargo.parent()?.join("rustc");
    rustc.is_file().then_some(rustc)
}

/// Strips rustup routing words from `args` and returns the selected toolchain.
///
/// Consumes the leading `+toolchain` or `run <toolchain>` selector and the
/// proxied `rustc` word so the resolved compiler receives only real flags.
fn strip_rustup_routing(args: &mut Vec<String>) -> Option<String> {
    let take = match args.first().map(String::as_str) {
        Some(first) if first.starts_with('+') => 1,
        Some("run") if args.len() >= 3 => 2,
        _ => 0,
    };
    let toolchain = if take > 0 {
        // The selector is `+toolchain` (one word) or `run <toolchain>` (two).
        let selector: Vec<String> = args.drain(..take).collect();
        Some(match take {
            2 => selector[1].clone(),
            _ => selector[0][1..].to_owned(),
        })
    } else {
        None
    };
    if args.first().is_some_and(|arg| arg == "rustc") {
        args.remove(0);
    }
    toolchain
}

fn tool_path(name: &str) -> Result<PathBuf> {
    let variable = match name {
        "cargo" => Some("GNRT_CARGO_BIN"),
        "gn" => Some("GNRT_GN_BIN"),
        "rustc" => Some("GNRT_RUSTC_BIN"),
        _ => None,
    };
    if let Some(value) = variable.and_then(std::env::var_os) {
        let value = PathBuf::from(value);
        if value.is_file() {
            return Ok(value);
        }
        if value.components().count() > 1 {
            bail!(
                "configured {name} executable is missing: {}",
                value.display()
            );
        }
        return find_on_path(&value.to_string_lossy());
    }
    find_on_path(name)
}

fn find_on_path(name: &str) -> Result<PathBuf> {
    if Path::new(name).is_file() {
        return Ok(PathBuf::from(name));
    }
    let search = std::env::var_os("PATH").context("PATH is not set")?;
    std::env::split_paths(&search)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .with_context(|| format!("{name} not found in PATH"))
}

fn check_exit_ok(output: &process::Output, description: &str) -> Result<()> {
    ensure!(
        output.status.success(),
        "{description} failed with {}: {}",
        exit_code_text(&output.status),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Formats a child process exit status for diagnostics.
pub fn exit_code_text(status: &process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "no exit code".to_owned(),
    }
}

/// Creates a directory and any missing parent directories.
///
/// # Errors
///
/// Returns an error if the directory cannot be created.
pub fn create_dirs_if_needed(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))
}

/// Formats GN arguments without evaluating them.
///
/// # Errors
///
/// Returns an error if GN cannot be run or the arguments are invalid.
pub fn normalize_gn_args(args: &[String]) -> Result<String> {
    let source = args.join(" ");
    if source.trim().is_empty() {
        return Ok(String::new());
    }
    let mut child = process::Command::new(tool_path("gn")?)
        .args(["format", "--stdin"])
        .stdin(process::Stdio::piped())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("GN formatter stdin unavailable")?
        .write_all(source.as_bytes())?;
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "invalid GN arguments: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let formatted = String::from_utf8(output.stdout)?;
    let mut lines = Vec::new();
    let mut skip_value = false;
    for line in formatted.lines() {
        let trimmed = line.trim();
        if skip_value {
            skip_value = false;
            continue;
        }
        if trimmed.starts_with("gnrt_mode =") {
            skip_value = trimmed.ends_with('=');
            continue;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("{}\n", lines.join("\n")))
    }
}

/// Joins a root-relative GN path label such as `//kernel/Cargo.lock` to the
/// checkout root.
///
/// # Errors
///
/// Returns an error if `label` is not a root-relative GN path.
pub fn gn_path_label_to_path(root: &Path, label: &str) -> Result<PathBuf> {
    let relative = label
        .strip_prefix("//")
        .with_context(|| format!("path {label} is not a root-relative GN path"))?;
    Ok(root.join(relative))
}
