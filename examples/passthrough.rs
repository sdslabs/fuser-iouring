use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::ffi::OsStrExt;
use tracing::info;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};
use fuser_iouring::ll::fuse_abi::*;
use fuser_iouring::{FileSystem, MountOption, Request};

fn add_dirent(buf: &mut Vec<u8>, ino: u64, off: u64, typ: u32, name: &[u8]) {
    let namelen = name.len() as u32;
    let dirent_size = std::mem::size_of::<FuseDirent>(); // 24 bytes
    let entry_len = dirent_size + name.len();

    // Calculate 8-byte padding needed
    let pad_len = ((entry_len + 7) & !7) - entry_len;

    let dirent = FuseDirent {
        ino,
        off,
        namelen,
        typ,
    };

    let dirent_slice = unsafe {
        std::slice::from_raw_parts(&dirent as *const _ as *const u8, dirent_size)
    };

    buf.extend_from_slice(dirent_slice);
    buf.extend_from_slice(name);
    buf.extend_from_slice(&vec![0u8; pad_len]);
}

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

    fn write(&self, _req: &Request, ino: u64, offset: u64, data: &[u8]) -> Result<u32, i32> {
        let path = self.get_path(ino).ok_or(libc::ENOENT)?;
        let mut file = OpenOptions::new().write(true).open(&path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        file.seek(SeekFrom::Start(0)).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        file.write_all(data).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        Ok(data.len() as u32)
    }

    fn mkdir(&self, _req: &Request, parent: u64, name: &[u8], mode: u32) -> Result<FuseEntryOut, i32> {
        let parent_path = self.get_path(parent).ok_or(libc::ENOENT)?;
        let child_path = parent_path.join(std::ffi::OsStr::from_bytes(name));

        // Creates directory on host OS
        fs::create_dir(&child_path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        // apply requested permissions (mode)
        fs::set_permissions(&child_path, fs::Permissions::from_mode(mode)).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        // Fetch new metadata and register the inode
        let meta = fs::symlink_metadata(&child_path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
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
    }

    fn create(&self, _req: &Request, parent: u64, name: &[u8], mode: u32) -> Result<(FuseEntryOut, FuseOpenOut), i32> {
        let parent_path = self.get_path(parent).ok_or(libc::ENOENT)?;
        let child_path = parent_path.join(std::ffi::OsStr::from_bytes(name));

        // creating the file
        fs::File::create(&child_path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        fs::set_permissions(&child_path, fs::Permissions::from_mode(mode)).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        let meta = fs::symlink_metadata(&child_path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        let new_ino = self.next_ino.fetch_add(1, Ordering::SeqCst);
        self.inodes.lock().unwrap().insert(new_ino, child_path);

        let entry = FuseEntryOut {
            nodeid: new_ino,
            generation: 1,
            entry_valid: 1,
            attr_valid: 1,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr: self.metadata_to_attr(&meta, new_ino),
        };

        // standard default flag for an opened file
        let open_out = FuseOpenOut{fh: 0, open_flags: 0, padding: 0};

        Ok((entry, open_out))
    }

    fn unlink(&self, _req: &Request, parent: u64, name: &[u8]) -> Result<(), i32> {
        let parent_path = self.get_path(parent).ok_or(libc::ENOENT)?;
        let child_path = parent_path.join(std::ffi::OsStr::from_bytes(name));

        fs::remove_file(&child_path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
    }

    fn rmdir(&self, _req: &Request, parent: u64, name: &[u8]) -> Result<(), i32> {
        let parent_path = self.get_path(parent).ok_or(libc::ENOENT)?;
        let child_path = parent_path.join(std::ffi::OsStr::from_bytes(name));

        fs::remove_dir(&child_path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
    }
    fn rename(&self, _req: &Request, parent: u64, name: &[u8], newparent: u64, newname: &[u8]) -> Result<(), i32> {
        let parent_path = self.get_path(parent).ok_or(libc::ENOENT)?;
        let new_parent_path = self.get_path(newparent).ok_or(libc::ENOENT)?;

        let old_path = parent_path.join(std::ffi::OsStr::from_bytes(name));
        let new_path = new_parent_path.join(std::ffi::OsStr::from_bytes(newname));

        fs::rename(&old_path, &new_path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
    }

    fn setattr(&self, _req: &Request, ino: u64, arg: &FuseSetAttrIn) -> Result<FuseAttrOut, i32> {
        let path = self.get_path(ino).ok_or(libc::ENOENT)?;

        use std::os::unix::ffi::OsStrExt;;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();


        // Truncate (file resizing)
        let valid = arg.valid;
        if (valid & FATTR_SIZE) != 0 {
            let file = OpenOptions::new().write(true).open(&path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
            file.set_len(arg.size).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        }

        // Chmod
        if (valid & FATTR_MODE) != 0 {
            unsafe {
                if libc::chmod(c_path.as_ptr(), arg.mode as libc::mode_t) != 0 {
                    return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
                }
            }
        }

        // Chown
        if (valid & FATTR_UID) != 0 || (valid & FATTR_GID) != 0 {
            let uid = if (valid & FATTR_UID) != 0 { arg.uid } else { u32::MAX };
            let gid = if (valid & FATTR_GID) != 0 { arg.gid } else { u32::MAX };

            unsafe {
                if libc::chown(c_path.as_ptr(), uid, gid) != 0 {
                    return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
                }
            }
        }

        // return updated metadata
        let meta = fs::symlink_metadata(&path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        Ok(FuseAttrOut {
            attr_valid: 1,
            attr_valid_nsec: 0,
            dummy: 0,
            attr: self.metadata_to_attr(&meta, ino),
        })
    }

    fn opendir(&self, _req: &Request, _ino: u64, _flags: u32) -> Result<FuseOpenOut, i32> {
        Ok(FuseOpenOut { fh: 0, open_flags: 0, padding: 0 })
    }

    fn readdir(&self, _req: &Request, ino: u64, offset: u64) -> Result<Vec<u8>, i32> {
        let path = self.get_path(ino).ok_or(libc::ENOENT)?;

        // Read physical directory contents from host OS
        let entries = fs::read_dir(&path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;

        let mut buf = Vec::new();
        let mut curr_offset = 0u64;

        for entry in entries {
            let entry = entry.map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
            curr_offset += 1;

            // Skip entries already served in previous read chunks
            if curr_offset <= offset {
                continue;
            }

            let file_name = entry.file_name();
            let name_bytes = file_name.as_bytes();
            let child_path = entry.path();

            // Retrieve existing Inode or allocate a new one
            let child_ino = {
                let mut inodes = self.inodes.lock().unwrap();
                let mut found = None;
                for (&k, v) in inodes.iter() {
                    if v == &child_path {
                        found = Some(k);
                        break;
                    }
                }
                match found {
                    Some(k) => k,
                    None => {
                        let new_ino = self.next_ino.fetch_add(1, Ordering::SeqCst);
                        inodes.insert(new_ino, child_path);
                        new_ino
                    }
                }
            };

            // Map standard POSIX dirent types
            let file_type = entry.file_type().ok().map(|t| {
                if t.is_dir() {
                    libc::DT_DIR as u32
                } else if t.is_symlink() {
                    libc::DT_LNK as u32
                } else {
                    libc::DT_REG as u32
                }
            }).unwrap_or(libc::DT_UNKNOWN as u32);

            // Pack into FUSE byte array
            add_dirent(&mut buf, child_ino, curr_offset, file_type, name_bytes);
        }

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