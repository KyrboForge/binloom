use crate::manifest::Manifest;
use anyhow::Result;
use std::path::Path;

pub(crate) fn list() -> Result<()> {
    let manifest = Manifest::try_from(Path::new("binloom.toml"))?;

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
