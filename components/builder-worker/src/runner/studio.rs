// Copyright (c) 2017 Chef Software Inc. and/or applicable contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, ATOMIC_USIZE_INIT, Ordering};

use hab_core::channel::{BLDR_CHANNEL_ENVVAR, STABLE_CHANNEL};
use hab_core::env;
use hab_core::fs;
use hab_core::url::BLDR_URL_ENVVAR;
use hab_core::AUTH_TOKEN_ENVVAR;

use error::{Error, Result};
use runner::log_pipe::LogPipe;
use runner::{NONINTERACTIVE_ENVVAR, RUNNER_DEBUG_ENVVAR};
use runner::workspace::Workspace;

pub static STUDIO_UID: AtomicUsize = ATOMIC_USIZE_INIT;
pub static STUDIO_GID: AtomicUsize = ATOMIC_USIZE_INIT;
pub const STUDIO_USER: &'static str = "krangschnak";
pub const STUDIO_GROUP: &'static str = "krangschnak";

lazy_static! {
    /// Absolute path to the Studio program
    static ref STUDIO_PROGRAM: PathBuf = fs::resolve_cmd_in_pkg(
        "hab-studio",
        include_str!(concat!(env!("OUT_DIR"), "/STUDIO_PKG_IDENT")),
    );

    pub static ref STUDIO_HOME: Mutex<PathBuf> = {
        Mutex::new(PathBuf::new())
    };
}

pub struct Studio<'a> {
    workspace: &'a Workspace,
    bldr_url: &'a str,
    auth_token: &'a str,
}

impl<'a> Studio<'a> {
    /// Creates a new Studio runner for a given `Workspace` and Builder URL.
    pub fn new(workspace: &'a Workspace, bldr_url: &'a str, auth_token: &'a str) -> Self {
        Studio {
            workspace,
            bldr_url,
            auth_token,
        }
    }

    /// Spawns a Studio build command, pipes output streams to the given `LogPipe` and returns the
    /// process' `ExitStatus`.
    ///
    /// # Errors
    ///
    /// * If the child process can't be spawned
    /// * If the calling thread can't wait on the child process
    /// * If the `LogPipe` fails to pipe output
    pub fn build(&self, log_pipe: &mut LogPipe) -> Result<ExitStatus> {
        let channel = if self.workspace.job.has_channel() {
            self.workspace.job.get_channel()
        } else {
            STABLE_CHANNEL
        };

        let mut cmd = Command::new("airlock");
        cmd.current_dir(self.workspace.src());
        cmd.uid(studio_uid());
        cmd.gid(studio_gid());
        cmd.env_clear();
        if let Some(val) = env::var_os(RUNNER_DEBUG_ENVVAR) {
            cmd.env("DEBUG", val);
        }
        cmd.env("HOME", &*STUDIO_HOME.lock().unwrap()); // Sets `$HOME` for build user
        cmd.env("USER", STUDIO_USER); // Sets `$USER` for build user
        cmd.env("PATH", env::var("PATH").unwrap_or(String::from(""))); // Sets `$PATH`
        cmd.env(NONINTERACTIVE_ENVVAR, "true"); // Disables progress bars
        cmd.env("TERM", "xterm-256color"); // Emits ANSI color codes
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.arg("run");
        cmd.arg(&*STUDIO_PROGRAM);
        cmd.arg("-k"); // Origin key
        cmd.arg(self.workspace.job.origin());
        cmd.arg("build");
        cmd.arg(build_path(self.workspace.job.get_project().get_plan_path()));
        debug!("building studio build command, cmd={:?}", &cmd);
        cmd.env("AIRLOCK_FS_ROOT", self.workspace.studio());
        debug!(
            "setting studio build command env, AIRLOCK_FS_ROOT={}",
            self.workspace.studio().display()
        );
        debug!(
            "setting studio build command env, {}={}",
            BLDR_CHANNEL_ENVVAR,
            &channel
        );
        cmd.env(BLDR_CHANNEL_ENVVAR, channel);
        debug!(
            "setting studio build command env, {}={}",
            BLDR_URL_ENVVAR,
            self.bldr_url
        );
        cmd.env(BLDR_URL_ENVVAR, self.bldr_url);
        cmd.env(AUTH_TOKEN_ENVVAR, self.auth_token);

        debug!("spawning studio build command");
        let mut child = cmd.spawn().map_err(|e| {
            Error::StudioBuild(self.workspace.studio().to_path_buf(), e)
        })?;
        log_pipe.pipe(&mut child)?;
        let exit_status = child.wait().map_err(|e| {
            Error::StudioBuild(self.workspace.studio().to_path_buf(), e)
        })?;
        debug!("completed studio build command, status={:?}", exit_status);
        Ok(exit_status)
    }
}

pub fn studio_gid() -> u32 {
    STUDIO_GID.load(Ordering::Relaxed) as u32
}

pub fn studio_uid() -> u32 {
    STUDIO_UID.load(Ordering::Relaxed) as u32
}

pub fn set_studio_gid(gid: u32) {
    STUDIO_GID.store(gid as usize, Ordering::Relaxed);
}

pub fn set_studio_uid(uid: u32) {
    STUDIO_UID.store(uid as usize, Ordering::Relaxed);
}

/// Returns a path argument suitable to pass to a Studio build command.
fn build_path(plan_path: &str) -> String {
    let mut parts: Vec<_> = plan_path.split("/").collect();
    if parts.last().map_or("", |p| *p) == "plan.sh" {
        parts.pop();
    }
    if parts.last().map_or("", |p| *p) == "habitat" {
        parts.pop();
    }

    if parts.is_empty() {
        String::from(".")
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::build_path;

    #[test]
    fn build_path_with_plan_sh() {
        assert_eq!(".", build_path("plan.sh"));
    }

    #[test]
    fn build_path_with_habitat_plan_sh() {
        assert_eq!(".", build_path("habitat/plan.sh"));
    }

    #[test]
    fn build_path_with_subdir_plan_sh() {
        assert_eq!("haaay", build_path("haaay/plan.sh"));
    }

    #[test]
    fn build_path_with_subdir_habitat_plan_sh() {
        assert_eq!(
            "components/yep",
            build_path("components/yep/habitat/plan.sh")
        );
    }
}
