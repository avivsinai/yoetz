use anyhow::{anyhow, Result};
use std::io::{self, Read};
use std::process::Command;

use crate::ApplyArgs;

pub(crate) fn handle_apply(args: ApplyArgs) -> Result<()> {
    let patch = if let Some(path) = args.patch_file {
        std::fs::read_to_string(path)?
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    };

    if patch.trim().is_empty() {
        return Err(anyhow!("patch is empty"));
    }

    let mut tmp = tempfile::NamedTempFile::new()?;
    use std::io::Write;
    tmp.write_all(patch.as_bytes())?;
    let tmp_path = tmp.into_temp_path();

    let mut cmd = Command::new("git");
    cmd.arg("apply");
    if args.check {
        cmd.arg("--check");
    }
    if args.reverse {
        cmd.arg("--reverse");
    }
    cmd.arg(tmp_path.as_ref() as &std::path::Path);

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git apply failed: {stderr}"));
    }

    if args.check {
        println!("Patch OK");
    } else {
        println!("Patch applied");
    }
    Ok(())
}
