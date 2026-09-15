use clap::Parser;
use fuser::{Config, SessionACL};
use log::{debug, error, info, trace, warn};
use std::fs::File;
use std::path::PathBuf;
use tempfile::env::temp_dir;
use zip::ZipArchive;

mod zip_fs;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// Archive to mount as a filesystem.
    archive: PathBuf,
    #[arg(short, long)]
    quiet: bool,
    /// More verbose logging
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Place to mount the filesystem
    mount_point: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut severity = log::Level::Info;

    for _ in 0..args.verbose {
        severity = severity.increment_severity();
    }

    stderrlog::new()
        .module(module_path!())
        .quiet(args.quiet)
        .verbosity(severity)
        .timestamp(stderrlog::Timestamp::Second)
        .init()
        .unwrap();

    let fs = zip_fs::ZipFS::new(&args.archive)?;
    let mut cfg = Config::default();
    cfg.acl = SessionACL::All;
    cfg.mount_options.push(fuser::MountOption::AutoUnmount);

    // for i in 0..archive.len() {
    //     let mut file = archive.by_index(i)?;
    //     info!("Filename: {}", file.name());
    //     // std::io::copy(&mut file, &mut std::io::stdout())?;
    // }
    //
    fuser::mount(fs, &args.mount_point, &cfg)?;

    info!("{:?}", args.archive);
    Ok(())
}
