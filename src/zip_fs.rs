use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, INodeNo, LockOwner, OpenFlags, ReplyAttr,
    ReplyData, ReplyDirectory, ReplyEntry, Request,
};
use log::{debug, error, info, trace, warn};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::path::PathBuf;
use zip::ZipArchive;

const TTL: Duration = Duration::from_secs(1); // 1 second
use std::time::Duration;
use std::time::UNIX_EPOCH;

pub struct ZipFS {
    z: ZipArchive<File>,
    files: HashMap<String, FileAttr>,
    dirs: HashSet<String>,
}

// fn get_directories(z: &ZipArchive<File>) -> anyhow::Result<HashSet<String>> {
//     let mut set: HashSet<String> = HashSet::new();

//     for entry in z.file_names() {
//         let mut s = entry.to_string();
//         let mut substr = s[0..];
//         let mut separator_loc: usize = 0;
//         while separator_loc != i32::MAX {
//             if let Some(loc) = s.rfind('/') {}
//         }
//     }

//     Ok(set)
// }
//
fn get_directories<R: std::io::Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
) -> HashSet<String> {
    let mut dirs = HashSet::new();

    for name in archive.file_names() {
        // Process all parent directories
        let mut path_so_far = String::new();
        for component in name.split('/') {
            if component.is_empty() {
                continue;
            }

            path_so_far.push_str(component);

            // Don't add the final component if it's a file (doesn't end with /)
            if name.ends_with('/') || path_so_far != name {
                dirs.insert(format!("{}/", path_so_far));
            }

            path_so_far.push('/');
        }
    }

    dirs.remove("");
    dirs
}

const ROOT_DIR_ATTR: FileAttr = FileAttr {
    ino: INodeNo::ROOT,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH, // 1970-01-01 00:00:00
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::Directory,
    perm: 0o755,
    nlink: 2,
    uid: 501,
    gid: 20,
    rdev: 0,
    flags: 0,
    blksize: 512,
};

fn get_files<R: std::io::Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
) -> HashMap<String, FileAttr> {
    let mut files = HashMap::new();

    files.insert(".".to_string(), ROOT_DIR_ATTR);
    files.insert("..".to_string(), ROOT_DIR_ATTR);

    files
}

impl ZipFS {
    pub fn new(p: &std::path::Path) -> anyhow::Result<Self> {
        let archive_file = File::open(p)?;

        let mut archive = ZipArchive::new(archive_file)?;
        let directories = get_directories(&mut archive);
        let files = get_files(&mut archive);
        info!("Directories: {:?}", directories);
        Ok(ZipFS {
            z: archive,
            dirs: directories,
        })
    }
}

const HELLO_DIR_ATTR: FileAttr = FileAttr {
    ino: INodeNo::ROOT,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH, // 1970-01-01 00:00:00
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::Directory,
    perm: 0o755,
    nlink: 2,
    uid: 501,
    gid: 20,
    rdev: 0,
    flags: 0,
    blksize: 512,
};

const HELLO_TXT_CONTENT: &str = "Hello World!\n";

const HELLO_TXT_ATTR: FileAttr = FileAttr {
    ino: INodeNo(2),
    size: 13,
    blocks: 1,
    atime: UNIX_EPOCH, // 1970-01-01 00:00:00
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::RegularFile,
    perm: 0o644,
    nlink: 1,
    uid: 501,
    gid: 20,
    rdev: 0,
    flags: 0,
    blksize: 512,
};

impl Filesystem for ZipFS {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        trace!("Looking up name {:?}", name);
        if u64::from(parent) == 1 && name.to_str() == Some("hello.txt") {
            reply.entry(&TTL, &HELLO_TXT_ATTR, fuser::Generation(0));
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        trace!("Looking up inode {ino}");
        match u64::from(ino) {
            1 => reply.attr(&TTL, &HELLO_DIR_ATTR),
            2 => reply.attr(&TTL, &HELLO_TXT_ATTR),
            _ => reply.error(Errno::ENOENT),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        _size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        trace!("Reading {_size} bytes from {ino} at offset {offset}");
        if u64::from(ino) == 2 {
            reply.data(&HELLO_TXT_CONTENT.as_bytes()[offset as usize..]);
        } else {
            reply.error(Errno::ENOENT);
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
        trace!("Reading directory {ino}");
        if u64::from(ino) != 1 {
            reply.error(Errno::ENOENT);
            return;
        }

        let entries = vec![
            (1, FileType::Directory, "."),
            (1, FileType::Directory, ".."),
            (2, FileType::RegularFile, "hello.txt"),
        ];

        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            // j = i;
            // i + 1 means the index of the next entry
            if reply.add(INodeNo(entry.0), (i + 1) as u64, entry.1, entry.2) {
                break;
            }
        }

        let mut j = 3;

        for file in self.z.file_names() {
            if file.matches('/').count() == 0 {
                if reply.add(
                    INodeNo(j as u64),
                    (j + 1) as u64,
                    FileType::RegularFile,
                    file,
                ) {
                    break;
                }
                j += 1;
            }
        }

        for directory in &self.dirs {
            if directory.matches('/').count() == 1 {
                if reply.add(
                    INodeNo(j as u64),
                    (j + 1) as u64,
                    FileType::Directory,
                    directory,
                ) {
                    break;
                }
            }
        }

        reply.ok();
    }
}
