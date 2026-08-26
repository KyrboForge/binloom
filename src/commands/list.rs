use crate::{
    common::{MANIFEST, project_root},
    domain::manifest::Manifest,
};
use anyhow::Result;

pub(crate) fn list() -> Result<()> {
    let root = project_root()?;
    let manifest_path = root.join(MANIFEST);
    let manifest = Manifest::try_from(manifest_path.as_path())?;

    println!("Binloom {}", manifest.binloom.version);

    if manifest.tools.is_empty() {
        println!("No tools configured.");
    }

    for (name, tool) in manifest.tools {
        println!("{name} {} ({})", tool.version, tool.source);

        if let Some(asset) = tool.asset {
            println!("  asset: {asset}");
        }
    }

    Ok(())
}
