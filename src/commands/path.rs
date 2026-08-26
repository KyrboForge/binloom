use crate::common::{TOOLS_DIR, project_root};
use anyhow::Result;

pub fn path() -> Result<()> {
    let path = project_root()?.join(TOOLS_DIR).join(".bin");

    println!("{}", path.display());

    Ok(())
}
