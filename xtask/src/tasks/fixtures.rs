use std::path::Path;

/// Generate large test fixtures under `target/`, never inside source trees.
pub fn run() -> Result<(), String> {
    let target = Path::new("target/test-fixtures");
    std::fs::create_dir_all(target).map_err(|err| err.to_string())?;
    std::fs::write(
        target.join("README.txt"),
        "Generated fixtures live here. This directory is ignored by git.\n",
    )
    .map_err(|err| err.to_string())?;
    println!("fixtures: {}", target.display());
    Ok(())
}
