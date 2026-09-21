use anyhow::bail;
use clap::Parser;
use fuser::{Config, MountOption};
use log::{Level, info};
use parse_size::parse_size;
use std::path::PathBuf;
use zipfs::ZipFs;

const DEFAULT_MAX_CACHE_SIZE: u64 = 64 * 1024 * 1024;

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

    #[arg(short, long)]
    /// Password to use to decrypt the archive.
    password: Option<String>,

    /// Open mount point in default file manager
    #[arg(short, long)]
    open: bool,

    /// Maximum size of the in-memory cache, default is 64mb
    #[arg(short, long, value_parser = |s: &str| parse_size(s).map_err(|e| e.to_string()), default_value_t = DEFAULT_MAX_CACHE_SIZE)]
    cache_size: u64,

    /// Silence all output
    #[arg(short, long)]
    quiet: bool,

    /// Increase the verbosity of output
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let mut level = Level::Info;

    for _ in 0..args.verbose {
        level = level.increment_severity();
    }

    stderrlog::new()
        .module(module_path!())
        .quiet(args.quiet)
        .verbosity(level)
        // .timestamp(ts)
        .init()
        .unwrap();

    if !args.archive.exists() {
        bail!("Archive {} does not exist", args.archive.display());
    }

    if !args.mountpoint.exists() || !args.mountpoint.is_dir() {
        bail!("Invalid mountpoint {}", args.mountpoint.display());
    }

    let fs = ZipFs::with_password_and_cache_size(&args.archive, args.password, args.cache_size)?;

    info!(
        "zipfs: mounting {} at {}",
        args.archive.display(),
        args.mountpoint.display()
    );

    let mut config = Config::default();
    config.acl = fuser::SessionACL::RootAndOwner;
    config.mount_options = vec![
        MountOption::FSName("zipfs".to_string()),
        MountOption::Subtype("zipfs".to_string()),
        MountOption::AutoUnmount,
    ];

    if args.open {
        open::that(&args.mountpoint)?;
    }

    fuser::mount(fs, &args.mountpoint, &config)?;

    Ok(())
}
