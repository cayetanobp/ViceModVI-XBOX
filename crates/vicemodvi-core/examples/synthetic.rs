//! Writes synthetic RPF9 and TTPF sources for manual testing:
//! `cargo run -p vicemodvi-core --example synthetic -- FOLDER`.

#[path = "../tests/support/mod.rs"]
mod support;

fn main() -> std::io::Result<()> {
    let folder = std::env::args_os().nth(1).map(std::path::PathBuf::from).unwrap_or_else(|| "synthetic".into());
    std::fs::create_dir_all(folder.join("data/ttdb"))?;
    std::fs::write(folder.join("data/ttdb/pool.ttpf"), support::pool(&[(256, 256, 71, 9, 0x1111), (64, 64, 28, 7, 0x2222)]))?;
    std::fs::write(folder.join("archive.rpf"), support::archive())?;
    std::fs::write(folder.join("props.rpf"), support::model_archive())?;
    println!("{}", folder.display());
    Ok(())
}
