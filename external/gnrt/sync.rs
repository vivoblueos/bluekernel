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

//! Collects GN declarations, resolves crates, and generates GN build rules.
//!
//! The current working directory is the project root and must contain `.gn`.

use crate::collect;
use crate::config::BuildConfig;
use crate::generate;
use crate::paths;
use crate::resolve;
use crate::SyncCommandArgs;
use anyhow::{bail, Context, Result};

/// Collects declarations, resolves crate features, and generates GN rules.
///
/// # Errors
///
/// Returns an error if the project root has no `.gn` file, configuration cannot
/// be loaded, or collection, resolution, or GN rule generation fails.
pub fn sync(args: SyncCommandArgs, paths: &dyn paths::GnrtPaths) -> Result<()> {
    if !paths.root().join(".gn").is_file() {
        bail!(
            "current directory {} does not contain a .gn file",
            paths.root().display()
        );
    }
    let build_out_dir = paths.root().join(&args.out);

    println!("Synchronizing crates and generating GN build rules ...");
    println!("  GN root: {}", paths.root().display());
    println!("  build output: {}", build_out_dir.display());

    let gnrt_dir = paths.gnrt_out_dir(&build_out_dir);

    let config = BuildConfig::from_path(paths.third_party_config_file())?;
    let collection_out_dir = gnrt_dir.join("collect");
    let mut context = collect::collect(
        &collection_out_dir,
        &gnrt_dir,
        args.target.as_deref(),
        &args.args,
        &config,
    )
    .context("GN declaration collection failed")?;
    resolve::resolve(&mut context, paths, &gnrt_dir)?;
    generate::generate(&context, paths).context("GN rule generation failed")?;

    println!("GN rule generation completed successfully.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_without_gn_file_fails() {
        struct NoGnPaths {
            root: std::path::PathBuf,
        }
        impl crate::paths::GnrtPaths for NoGnPaths {
            fn root(&self) -> &std::path::Path {
                &self.root
            }
            fn third_party(&self) -> &std::path::Path {
                panic!("not under test")
            }
            fn third_party_cargo_root(&self) -> &std::path::Path {
                panic!("not under test")
            }
            fn third_party_config_file(&self) -> &std::path::Path {
                panic!("not under test")
            }
            fn get_vendor_dir_for_package(
                &self,
                _name: &str,
                _version: &semver::Version,
            ) -> std::path::PathBuf {
                panic!("not under test")
            }
            fn get_build_dir_for_package(
                &self,
                _name: &str,
                _version: &semver::Version,
            ) -> std::path::PathBuf {
                panic!("not under test")
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let paths = NoGnPaths {
            root: dir.path().to_path_buf(),
        };
        let args = SyncCommandArgs {
            out: "out".into(),
            all: false,
            target: None,
            args: vec![],
        };
        let err = sync(args, &paths).expect_err("sync without .gn should fail");
        assert!(err.to_string().contains(".gn"), "got: {err}");
    }
}
