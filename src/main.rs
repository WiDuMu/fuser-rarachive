use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    MountOption, OpenAccMode, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, ReplyStatfs, Request,
};
use zip::ZipArchive;

const TTL: Duration = Duration::from_secs(1);
const ROOT_INO: u64 = 1;
const BLOCK_SIZE: u64 = 512;

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

/// In-memory directory entry.
#[derive(Clone)]
struct Node {
    ino: u64,
    parent: u64,
    name: String,
    kind: FileType,
    size: u64,
    mtime: SystemTime,
    /// Index into the ZipArchive for regular files; None for directories.
    file_index: Option<usize>,
    children: HashMap<String, u64>,
}

struct ZipFs {
    nodes: HashMap<u64, Node>,
    archive: Mutex<ZipArchive<File>>,
    /// Lazily populated, decompressed file contents, keyed by inode.
    cache: Mutex<HashMap<u64, Arc<Vec<u8>>>>,
}

struct EntryInfo {
    path: String,
    is_dir: bool,
    size: u64,
    mtime: SystemTime,
    index: usize,
}

impl ZipFs {
    fn new(archive_path: &PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let file = File::open(archive_path)?;
        let mut archive = ZipArchive::new(file)?;

        // First pass: copy the metadata out so we drop the mutable borrow
        // on the archive before we start mutating the tree.
        let mut infos: Vec<EntryInfo> = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let info = {
                let entry = archive.by_index(index)?;
                let path = entry.name().trim_start_matches("./").to_string();
                if path.is_empty() {
                    continue;
                }
                EntryInfo {
                    path,
                    is_dir: entry.is_dir(),
                    size: entry.size(),
                    mtime: entry
                        .last_modified()
                        .map(zip_datetime_to_system_time)
                        .unwrap_or_else(SystemTime::now),
                    index,
                }
            };
            infos.push(info);
        }

        let mut nodes = HashMap::new();
        nodes.insert(
            ROOT_INO,
            Node {
                ino: ROOT_INO,
                parent: ROOT_INO,
                name: String::new(),
                kind: FileType::Directory,
                size: 0,
                mtime: SystemTime::now(),
                file_index: None,
                children: HashMap::new(),
            },
        );
        let mut next_ino: u64 = ROOT_INO + 1;

        for info in infos {
            insert_entry(&mut nodes, &mut next_ino, info);
        }

        Ok(ZipFs {
            nodes,
            archive: Mutex::new(archive),
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Lazily decompresses (once) and returns the full contents of a file.
    fn file_data(&self, ino: u64, file_index: usize) -> std::io::Result<Arc<Vec<u8>>> {
        {
            let cache = self.cache.lock().unwrap();
            if let Some(data) = cache.get(&ino) {
                return Ok(data.clone());
            }
        }
        // Hold the archive lock only while decompressing.
        let mut archive = self.archive.lock().unwrap();
        let mut file = archive.by_index(file_index)?;
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        let data = Arc::new(buf);
        self.cache.lock().unwrap().insert(ino, data.clone());
        Ok(data)
    }
}

fn insert_entry(nodes: &mut HashMap<u64, Node>, next_ino: &mut u64, info: EntryInfo) {
    let mut parts: Vec<&str> = info.path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return;
    }

    // For files, the last component is the file itself; for directory
    // entries, every component is part of the path.
    let file_leaf: Option<&str> = if info.is_dir {
        None
    } else {
        Some(parts.pop().unwrap())
    };

    let parent = ensure_dir_path(nodes, &parts, next_ino, info.mtime);

    if let Some(leaf) = file_leaf {
        let parent_node = nodes.get_mut(&parent).expect("parent node must exist");
        if parent_node.children.contains_key(leaf) {
            eprintln!("zipfs: skipping duplicate zip entry '{}'", info.path);
            return;
        }
        let ino = *next_ino;
        *next_ino += 1;
        parent_node.children.insert(leaf.to_string(), ino);
        nodes.insert(
            ino,
            Node {
                ino,
                parent,
                name: leaf.to_string(),
                kind: FileType::RegularFile,
                size: info.size,
                mtime: info.mtime,
                file_index: Some(info.index),
                children: HashMap::new(),
            },
        );
    }
}

/// Walks (creating as needed) the directory chain described by `parts`,
/// starting at the root, and returns the inode of the final directory.
fn ensure_dir_path(
    nodes: &mut HashMap<u64, Node>,
    parts: &[&str],
    next_ino: &mut u64,
    mtime: SystemTime,
) -> u64 {
    let mut current = ROOT_INO;
    for part in parts {
        let existing = nodes
            .get(&current)
            .and_then(|n| n.children.get(*part))
            .copied();
        let child = match existing {
            Some(ino) => ino,
            None => {
                let ino = *next_ino;
                *next_ino += 1;
                nodes.insert(
                    ino,
                    Node {
                        ino,
                        parent: current,
                        name: (*part).to_string(),
                        kind: FileType::Directory,
                        size: 0,
                        mtime,
                        file_index: None,
                        children: HashMap::new(),
                    },
                );
                let parent_node = nodes.get_mut(&current).expect("directory must exist");
                parent_node.children.insert((*part).to_string(), ino);
                ino
            }
        };
        current = child;
    }
    current
}

/// Converts a zip::DateTime to SystemTime without pulling in chrono.
/// Uses Howard Hinnant's civil-from-days algorithm.
fn zip_datetime_to_system_time(dt: zip::DateTime) -> SystemTime {
    let (y, m, d) = (dt.year() as i64, dt.month() as i64, dt.day() as i64);
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146_097 + doe - 719_468;

    let secs =
        days * 86_400 + dt.hour() as i64 * 3_600 + dt.minute() as i64 * 60 + dt.second() as i64;

    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH
    }
}

fn make_attr(uid: u32, gid: u32, node: &Node) -> FileAttr {
    let is_dir = matches!(node.kind, FileType::Directory);
    FileAttr {
        ino: INodeNo(node.ino),
        size: node.size,
        blocks: (node.size + BLOCK_SIZE - 1) / BLOCK_SIZE,
        atime: node.mtime,
        mtime: node.mtime,
        ctime: node.mtime,
        crtime: node.mtime,
        kind: node.kind,
        perm: if is_dir { 0o555 } else { 0o444 },
        nlink: if is_dir { 2 } else { 1 },
        uid,
        gid,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

impl Filesystem for ZipFs {
    fn lookup(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = match name.to_str() {
            Some(n) => n,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let child_ino = match self.nodes.get(&parent.0).and_then(|p| p.children.get(name)) {
            Some(&ino) => ino,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let node = match self.nodes.get(&child_ino) {
            Some(n) => n,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let attr = make_attr(req.uid(), req.gid(), node);
        reply.entry(&TTL, &attr, Generation(0));
    }

    fn getattr(&self, req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.nodes.get(&ino.0) {
            Some(node) => {
                let attr = make_attr(req.uid(), req.gid(), node);
                reply.attr(&TTL, &attr);
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let node = match self.nodes.get(&ino.0) {
            Some(n) if matches!(n.kind, FileType::Directory) => n,
            _ => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        // "." and ".." first, then children sorted by name for stability.
        let mut entries: Vec<(u64, FileType, String)> = Vec::with_capacity(node.children.len() + 2);
        entries.push((node.ino, FileType::Directory, ".".to_string()));
        entries.push((node.parent, FileType::Directory, "..".to_string()));
        for (name, &child_ino) in &node.children {
            let kind = match self.nodes.get(&child_ino) {
                Some(c) => c.kind,
                None => FileType::RegularFile,
            };
            entries.push((child_ino, kind, name.clone()));
        }
        entries.sort_by(|a, b| a.2.cmp(&b.2));

        for (i, (entry_ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            let buffer_full = reply.add(INodeNo(*entry_ino), i as u64 + 1, *kind, OsStr::new(name));
            if buffer_full {
                break;
            }
        }
        reply.ok();
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.nodes.get(&ino.0) {
            Some(node) if matches!(node.kind, FileType::RegularFile) => {
                if flags.acc_mode() != OpenAccMode::O_RDONLY {
                    reply.error(Errno::EACCES);
                } else {
                    // Stateless: no per-open bookkeeping, dummy handle.
                    reply.opened(FileHandle(0), FopenFlags::empty());
                }
            }
            Some(_) => reply.error(Errno::EISDIR),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let file_index = match self.nodes.get(&ino.0) {
            Some(node) if matches!(node.kind, FileType::Directory) => {
                reply.error(Errno::EISDIR);
                return;
            }
            Some(node) => match node.file_index {
                Some(idx) => idx,
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            },
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        let data = match self.file_data(ino.0, file_index) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("zipfs: failed to read zip entry: {}", e);
                reply.error(Errno::EIO);
                return;
            }
        };

        let offset = offset as usize;
        if offset >= data.len() {
            reply.data(&[]);
            return;
        }
        let end = (offset.saturating_add(size as usize)).min(data.len());
        reply.data(&data[offset..end]);
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let total_size: u64 = self.nodes.values().map(|n| n.size).sum();
        let blocks = (total_size + BLOCK_SIZE - 1) / BLOCK_SIZE;
        let files = self.nodes.len() as u64;
        reply.statfs(blocks, blocks, blocks, files, files, 512, 255, 512);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let fs = ZipFs::new(&args.archive)?;

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
