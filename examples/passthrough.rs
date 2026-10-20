use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::ffi::OsStrExt;
use tracing::info;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};
use fuser_iouring::ll::fuse_abi::*;
use fuser_iouring::{FileSystem, MountOption, Request};

pub struct PassthroughFS {
    data_dir: PathBuf,
    // Maps FUSE u64 Inodes back to actual Host PathBufs
    inodes: Mutex<HashMap<u64, PathBuf>>,
    next_ino: AtomicU64,
}

impl PassthroughFS {
    pub fn new(data_dir: &Path) -> Self {
        let mut inodes = HashMap::new();
        // inode 1 for root mount directory
        inodes.insert(1, data_dir.to_path_buf());

        Self {
            data_dir: data_dir.to_path_buf(),
            inodes: Mutex::new(inodes),
            next_ino: AtomicU64::new(2),
        }
    }

    fn get_path(&self, ino: u64) -> Option<PathBuf> {
        self.inodes.lock().unwrap().get(&ino).cloned()
    }

    fn metadata_to_attr(&self, meta: &fs::Metadata, ino: u64) -> FuseAttr {
        FuseAttr {
            ino,
            size: meta.size(),
            blocks: meta.blocks(),
            atime: meta.atime() as u64,
            mtime: meta.mtime() as u64,
            ctime: meta.ctime() as u64,
            atimensec: meta.atime_nsec() as u32,
            mtimensec: meta.mtime_nsec() as u32,
            ctimensec: meta.ctime_nsec() as u32,
            mode: meta.mode(),
            nlink: meta.nlink() as u32,
            uid: meta.uid(),
            gid: meta.gid(),
            rdev: meta.rdev() as u32,
            blksize: meta.blksize() as u32,
            padding: 0,
        }
    }
}

impl FileSystem for PassthroughFS {
    fn getattr(&self, _req: &Request, ino: u64) -> Result<FuseAttrOut, i32> {
        let path = self.get_path(ino).ok_or(libc::ENOENT)?;

        // Passing call down to the host OS
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                Ok(FuseAttrOut {
                    attr_valid: 1, // Cache for 1 sec
                    attr_valid_nsec: 0,
                    dummy: 0,
                    attr: self.metadata_to_attr(&meta, ino),
                })
            },
            Err(e) => Err(e.raw_os_error().unwrap_or(libc::EIO)),
        }
    }

    fn lookup(&self, _req: &Request, parent: u64, name: &[u8]) -> Result<FuseEntryOut, i32> {
        let parent_path = self.get_path(parent).ok_or(libc::ENOENT)?;

        use std::ffi::OsStr;
        let os_name = OsStr::from_bytes(name);
        let child_path = parent_path.join(&os_name);

        match fs::symlink_metadata(&child_path) {
            Ok(meta) => {
                let new_ino = self.next_ino.fetch_add(1, Ordering::SeqCst);
                self.inodes.lock().unwrap().insert(new_ino, child_path);

                Ok(FuseEntryOut {
                    nodeid: new_ino,
                    generation: 1,
                    entry_valid: 1,
                    attr_valid: 1,
                    entry_valid_nsec: 0,
                    attr_valid_nsec: 0,
                    attr: self.metadata_to_attr(&meta, new_ino),
                })
            },
            Err(e) => Err(e.raw_os_error().unwrap_or(libc::ENOENT)),
        }
    }

    fn read(&self, _req: &Request, ino: u64, offset: u64, size: u32) -> Result<Vec<u8>, i32> {
        let path = self.get_path(ino).ok_or(libc::ENOENT)?;

        let mut file = fs::File::open(path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        file.seek(SeekFrom::Start(offset)).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        let mut buf = vec![0; size as usize];
        let bytes_read = file.read(&mut buf).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        buf.truncate(bytes_read);
        Ok(buf)
    }
}

fn main() {
    info!("Starting PassthroughFS");

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: cargo run --example passthrough <mountpoint> <data_dir>");
        return;
    }

    let mountpoint = &args[1];
    let data_dir_string = if args.len() > 2 {
        args[2].clone()
    } else {
        "/tmp/fuser_data_passthrough".to_string()
    };

    let data_dir = Path::new(&data_dir_string);
    println!("mounted PassthroughFS on {} (Data: {:?})", mountpoint, data_dir);

    let fs = PassthroughFS::new(&data_dir);

    if let Err(e) = fuser_iouring::mount(fs, mountpoint, &[MountOption::AutoUnmount]) {
        eprintln!("Error: {}", e);
    }
}