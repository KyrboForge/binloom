use std::env;

use anyhow::{Context, Result};

pub fn path() -> Result<()> {
    let path = env::current_dir()
        .context("failed to determine current directory")?
        .join(".tools/.bin");

    println!("{}", path.display());

    Ok(())
}
