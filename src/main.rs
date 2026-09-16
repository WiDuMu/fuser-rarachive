use anyhow::bail;
use clap::Parser;
use fuser::{Config, MountOption};
use std::path::PathBuf;
mod zip_fs;

#[derive(Parser)]
#[command(
    name = "zipfs",
    version,
    about = "Mount a zip archive as a read-only FUSE filesystem"
)]
struct Args {
    /// Path to the zip archive to expose
    archive: PathBuf,

    /// Directory to mount the archive on (must exist and be empty)
    mountpoint: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if !args.archive.exists() {
        bail!("Archive {} does not exist", args.archive.display());
    }

    if !args.mountpoint.exists() || !args.mountpoint.is_dir() {
        bail!("Invalid mountpoint {}", args.mountpoint.display());
    }

    let fs = zip_fs::ZipFs::new(&args.archive)?;

    println!(
        "zipfs: mounting {} at {} (Ctrl+C or fusermount -u to unmount)",
        args.archive.display(),
        args.mountpoint.display()
    );

    let mut config = Config::default();
    config.acl = fuser::SessionACL::RootAndOwner;
    config.mount_options = vec![
        MountOption::FSName("zipfs".to_string()),
        MountOption::Subtype("zipfs".to_string()),
        // MountOption::ReadOnly,
        MountOption::AutoUnmount,
    ];

    fuser::mount(fs, &args.mountpoint, &config)?;

    Ok(())
}
