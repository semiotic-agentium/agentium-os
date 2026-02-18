use std::path::PathBuf;
use std::process::Command;

pub struct CliHarness {
    cargo_dir: PathBuf,
}

impl Default for CliHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl CliHarness {
    pub fn new() -> Self {
        Self {
            cargo_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|path| path.parent())
                .expect("test-support crate should be under crates/")
                .to_path_buf(),
        }
    }

    pub fn builder_command(&self) -> Command {
        let mut command = Command::new("cargo");
        command
            .current_dir(&self.cargo_dir)
            .arg("run")
            .arg("--quiet")
            .arg("-p")
            .arg("baml-rt-builder")
            .arg("--bin")
            .arg("baml-agent-builder")
            .arg("--");
        command
    }
}
