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

//! Process and filesystem helpers for GN declaration collection.

use anyhow::{bail, ensure, Context, Result};
use std::io::Write;
use std::{
    fs,
    path::{Path, PathBuf},
    process,
};

fn tool_path(name: &str) -> Result<PathBuf> {
    let variable = match name {
        "gn" => Some("GNRT_GN_BIN"),
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
