use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tracing::{info, error};

use fuser_iouring::ll::fuse_abi::*;
use fuser_iouring::{FileSystem, MountOption, Request};

const BLOCK_SIZE: u32 = 512;

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Debug)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InodeAttributes {
    pub inode: u64,
    pub size: u64,
    pub kind: FileKind,
    pub mode: u16,
    pub hardlinks: u32,
    pub uid: u32,
    pub gid: u32,
    pub atime: (i64, u32),
    pub mtime: (i64, u32),
    pub ctime: (i64, u32),
}

impl InodeAttributes {
    pub fn to_fuse_attr(&self) -> FuseAttr {
        FuseAttr{
            ino : self.inode,
            size: self.size,
            blocks : (self.size + 511) / 512,
            atime : self.atime.0 as u64,
            mtime : self.mtime.0 as u64,
            ctime : self.ctime.0 as u64,
            atimensec : self.atime.1,
            mtimensec : self.mtime.1,
            ctimensec : self.ctime.1,
            mode : self.mode as u32,
            nlink : self.hardlinks,
            uid : self.uid,
            gid : self.gid,
            rdev : 0,
            blksize : BLOCK_SIZE,
            padding: 0,
        }
    }
}

pub struct SimpleFS {
    data_dir : PathBuf,
}

impl SimpleFS {
    pub fn new(data_dir: &str) -> Self {
        let path = PathBuf::from(data_dir);
        fs::create_dir_all(path.join("inodes")).unwrap();
        fs::create_dir_all(path.join("contents")).unwrap();
        let fs = SimpleFS {data_dir : path};

        if fs.get_inode(1).is_err() {
            let now = time_now();
            let root = InodeAttributes {
                inode : 1,
                size : 2,
                kind : FileKind::Directory,
                mode : 0o40755,
                hardlinks : 2,
                uid : 1000,
                gid : 1000,
                atime : now,
                mtime : now,
                ctime : now,
            };
            fs.write_inode(&root);

            let mut entries = BTreeMap::new();
            entries.insert(b".".to_vec(), (1, FileKind::Directory));
            entries.insert(b"..".to_vec(), (1, FileKind::Directory));
            fs.write_directory_content(1, &entries);
        }
        fs
    }

    fn allocate_next_inode(&self) -> u64 {
        let path = self.data_dir.join("superblock");
        let current: u64 = if let Ok(f) = File::open(&path) {
            bincode::deserialize_from(f).unwrap_or(1)
        } else { 1 };
        let next = current + 1;
        let f = File::create(&path).unwrap();
        bincode::serialize_into(f, &next).unwrap();
        next
    }

    fn inode_path(&self, ino: u64) -> PathBuf { self.data_dir.join("inodes").join(ino.to_string()) }
    fn content_path(&self, ino: u64) -> PathBuf { self.data_dir.join("contents").join(ino.to_string()) }

    pub fn get_inode(&self, ino: u64) -> Result<InodeAttributes , i32> {
        match File::open(self.inode_path(ino)) {
            Ok(f) => Ok(bincode::deserialize_from(f).map_err(|_| libc::EIO)?),
            Err(e) => {
                eprintln!("Failed to describe the inode {} : {}" , ino , e);
                Err(libc::ENOENT)
            }
        }
    }

    fn write_inode(&self, attr: &InodeAttributes) {
        let f = File::create(self.inode_path(attr.inode)).unwrap();
        bincode::serialize_into(f, attr).unwrap();
    }

    fn get_directory_content(&self, ino: u64) -> Result<BTreeMap<Vec<u8> , (u64, FileKind)> , i32> {
        match File::open(self.content_path(ino)) {
            Ok(f) => {
                match bincode::deserialize_from(f) {
                    Ok(map) => Ok(map),
                    Err(e) => {
                        eprintln!("CRITICAL: Corrupt directory content for inode {} : {}" , ino, e);
                        Err(libc::EIO)
                    }
                }
            }
            Err(_) => Err(libc::ENOENT),
        }
    }

    fn write_directory_content(&self, ino: u64, map: &BTreeMap<Vec<u8>, (u64, FileKind)>) {
        let f = File::create(self.content_path(ino)).unwrap();
        bincode::serialize_into(f, map).unwrap();
    }
}

impl FileSystem for SimpleFS {
    fn lookup(&self, _req: &Request, parent: u64, name: &[u8]) -> Result<FuseEntryOut , i32> {
        let dir = self.get_directory_content(parent)?;
        if let Some((ino, _)) = dir.get(name) {
            let attr = self.get_inode(*ino)?;
            Ok(FuseEntryOut{
                nodeid : attr.inode,
                generation : 1,
                entry_valid: 0,
                attr_valid : 0,
                entry_valid_nsec: 0,
                attr_valid_nsec: 0,
                attr: attr.to_fuse_attr(),
            })
        } else {
            Err(libc::ENOENT)
        }
    }

    fn getattr(&self, _req: &Request, ino: u64) -> Result<FuseAttrOut, i32> {
        let attr = self.get_inode(ino)?;
        Ok(FuseAttrOut {
            attr_valid: 0,
            attr_valid_nsec: 0,
            dummy: 0,
            attr: attr.to_fuse_attr(),
        })
    }

    fn setattr(&self, _req: &Request, ino: u64, arg: &FuseSetAttrIn) -> Result<FuseAttrOut, i32> {
        let mut attr = self.get_inode(ino)?;

        if arg.valid & FATTR_MODE != 0 {
            attr.mode = (attr.mode & !0o7777) | (arg.mode as u16 & 0o7777);
        }
        if arg.valid & FATTR_UID != 0 {
            attr.uid = arg.uid;
        }
        if arg.valid & FATTR_GID != 0 {
            attr.gid = arg.gid;
        }
        if arg.valid & FATTR_SIZE != 0 {
            attr.size = arg.size;
            // Truncate the actual backing file
            if let Ok(f) = OpenOptions::new().write(true).open(self.content_path(ino)) {
                let _ = f.set_len(arg.size);
            }
        }
        if arg.valid & FATTR_ATIME != 0 {
            attr.atime = (arg.atime as i64, arg.atimensec);
        }
        if arg.valid & FATTR_MTIME != 0 {
            attr.mtime = (arg.mtime as i64, arg.mtimensec);
        }

        self.write_inode(&attr);

        Ok(FuseAttrOut {
            attr_valid: 0,
            attr_valid_nsec: 0,
            dummy: 0,
            attr: attr.to_fuse_attr(),
        })
    }

    fn mkdir(&self, _req: &Request, parent: u64, name: &[u8], mode: u32) -> Result<FuseEntryOut, i32> {
        let mut parent_attr = self.get_inode(parent)?;
        if parent_attr.kind != FileKind::Directory { return Err(libc::ENOTDIR); }

        let ino = self.allocate_next_inode();
        let now = time_now();

        let new_attr = InodeAttributes {
            inode: ino,
            size : 2,
            kind : FileKind::Directory,
            mode : (mode & 0o777) as u16 | 0o40000,
            hardlinks : 2,
            uid : 1000,
            gid : 1000,
            atime : now,
            mtime : now,
            ctime : now,
        };
        self.write_inode(&new_attr);

        let mut new_dir_map = BTreeMap::new();
        new_dir_map.insert(b".".to_vec(), (ino, FileKind::Directory));
        new_dir_map.insert(b"..".to_vec(), (parent, FileKind::Directory));
        self.write_directory_content(ino, &new_dir_map);

        let mut parent_map = self.get_directory_content(parent)?;
        if parent_map.contains_key(name) { return Err(libc::EEXIST); }
        parent_map.insert(name.to_vec(), (ino, FileKind::Directory));
        self.write_directory_content(parent, &parent_map);

        parent_attr.mtime = now;
        parent_attr.size = parent_map.len() as u64;
        self.write_inode(&parent_attr);

        Ok(FuseEntryOut {
            nodeid: ino,
            generation: 1,
            entry_valid: 0,
            attr_valid: 0,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr: new_attr.to_fuse_attr(),
        })
    }

    fn create(&self, _req: &Request, parent: u64, name: &[u8], mode: u32) -> Result<(FuseEntryOut, FuseOpenOut), i32> {
        let mut parent_attr = self.get_inode(parent)?;
        let ino = self.allocate_next_inode();
        let now = time_now();

        let new_attr = InodeAttributes {
            inode : ino,
            size : 0,
            kind: FileKind::File,
            mode: (mode & 0o7777) as u16 | 0o100000,
            hardlinks: 1,
            uid : 1000,
            gid : 1000,
            atime : now,
            mtime : now,
            ctime : now,
        };
        self.write_inode(&new_attr);

        File::create(self.content_path(ino)).map_err(|_| libc::EIO)?;

        let mut parent_map = self.get_directory_content(parent)?;
        if parent_map.contains_key(name) { return Err(libc::EEXIST); }
        parent_map.insert(name.to_vec(), (ino, FileKind::File));
        self.write_directory_content(parent, &parent_map);

        parent_attr.mtime = now;
        parent_attr.size = parent_map.len() as u64;
        self.write_inode(&parent_attr);

        Ok((
            FuseEntryOut{
                nodeid : ino,
                generation: 1,
                entry_valid : 0,
                attr_valid : 0,
                entry_valid_nsec : 0,
                attr_valid_nsec : 0,
                attr: new_attr.to_fuse_attr(),
            },
            FuseOpenOut {
                fh : ino,
                open_flags: 0,
                padding: 0
            }
            ))
    }

    fn write(&self, _req: &Request, ino: u64 , offset: u64, data: &[u8]) -> Result<u32 , i32> {
        let mut attr = self.get_inode(ino)?;
        if attr.kind != FileKind::File { return Err(libc::EISDIR); }

        let mut f = OpenOptions::new().write(true).open(self.content_path(ino)).map_err(|_| libc::ENOENT)?;
        f.seek(SeekFrom::Start(offset)).map_err(|_| libc::EIO)?;
        f.write_all(data).map_err(|_| libc::EIO)?;

        let end_pos = offset + data.len() as u64;
        if end_pos > attr.size {
            attr.size = end_pos;
            attr.mtime = time_now();
            self.write_inode(&attr);
        }
        Ok(data.len() as u32)
    }

    fn read(&self, _req: &Request, ino: u64 , offset: u64, size: u32) -> Result<Vec<u8> , i32> {
        let attr = self.get_inode(ino)?;
        let mut f = File::open(self.content_path(ino)).map_err(|_| libc::ENOENT)?;

        if offset >= attr.size { return Ok(Vec::new()); }
        f.seek(SeekFrom::Start(offset)).map_err(|_| libc::EIO)?;

        let mut buf = vec![0u8; size as usize];
        let n = f.read(&mut buf).map_err(|_| libc::EIO)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn readdir(&self, _req: &Request, ino: u64, offset: u64) -> Result<Vec<u8> , i32> {
        let entries = self.get_directory_content(ino)?;
        let mut reply_buf = Vec::new();

        for(i , (name , (child_ino , kind))) in entries.iter().enumerate().skip(offset as usize) {
            let typ = match kind {
                FileKind::Directory => libc::DT_DIR,
                FileKind::File => libc::DT_REG,
                _ => libc::DT_UNKNOWN,
            } as u32;

            let dirent_len = std::mem::size_of::<FuseDirent>();
            let name_len = name.len();
            let align = (dirent_len + name_len + 7) & !7;
            let padding = align - (dirent_len + name_len);

            let ent = FuseDirent {
                ino: *child_ino,
                off: (i+1) as u64,
                namelen : name_len as u32,
                typ,
            };

            let p = &ent as *const _ as *const u8;
            reply_buf.extend_from_slice(unsafe { std::slice::from_raw_parts(p, dirent_len) });
            reply_buf.extend_from_slice(name);
            reply_buf.extend(std::iter::repeat(0).take(padding));
        }
        Ok(reply_buf)
    }

    fn readdirplus(&self, _req: &Request, ino: u64 , offset: u64) -> Result<Vec<u8> , i32> {
        let entries = self.get_directory_content(ino)?;
        let mut reply_buf = Vec::new();

        for (i , (name , (child_ino , kind))) in entries.iter().enumerate().skip(offset as usize) {
            let typ = match kind {
                FileKind::Directory => libc::DT_DIR,
                FileKind::File => libc::DT_REG,
                _ => libc::DT_UNKNOWN,
            } as u32;

            let attr = match self.get_inode(*child_ino) {
                Ok(a) => a,
                Err(_) => continue,
            };

            let entry_out = FuseEntryOut {
                nodeid: attr.inode,
                generation: 1,
                entry_valid: 0,
                attr_valid: 0,
                entry_valid_nsec: 0,
                attr_valid_nsec: 0,
                attr: attr.to_fuse_attr(),
            };

            let dirent = FuseDirent {
                ino: *child_ino,
                off: (i+1) as u64,
                namelen: name.len() as u32,
                typ,
            };

            let entry_len = std::mem::size_of::<FuseEntryOut>();
            let dirent_len = std::mem::size_of::<FuseDirent>();
            let name_len = name.len();

            let total_len = entry_len + dirent_len + name_len;
            let align = (total_len + 7) & !7;
            let padding = align - total_len;

            let p_entry = &entry_out as *const _ as *const u8;
            reply_buf.extend_from_slice(unsafe { std::slice::from_raw_parts(p_entry, entry_len) });

            let p_dirent = &dirent as *const _ as *const u8;
            reply_buf.extend_from_slice(unsafe { std::slice::from_raw_parts(p_dirent, dirent_len) });

            reply_buf.extend_from_slice(name);
            reply_buf.extend(std::iter::repeat(0).take(padding));
        }
        Ok(reply_buf)
    }

    fn unlink(&self, _req: &Request, parent: u64, name: &[u8]) -> Result<(), i32> {
        let mut parent_map = self.get_directory_content(parent)?;

        if let Some(&(child_ino, kind)) = parent_map.get(name) {
            if kind == FileKind::Directory {
                return Err(libc::EISDIR);
            }

            parent_map.remove(name);
            self.write_directory_content(parent, &parent_map);

            let _ = fs::remove_file(self.inode_path(child_ino));
            let _ = fs::remove_dir_all(self.content_path(child_ino));

            if let Ok(mut parent_attr) = self.get_inode(parent) {
                parent_attr.mtime = time_now();
                parent_attr.size = parent_map.len() as u64;
                self.write_inode(&parent_attr);
            }

            Ok(())
        } else {
            Err(libc::ENOENT)
        }
    }

    fn rmdir(&self, _req: &Request, parent: u64, name: &[u8]) -> Result<(), i32> {
        let mut parent_map = self.get_directory_content(parent)?;

        if let Some(&(child_ino, kind)) = parent_map.get(name) {
            if kind != FileKind::Directory {
                return Err(libc::ENOTDIR);
            }

            let child_map = self.get_directory_content(child_ino)?;
            if child_map.len() > 2 {
                return Err(libc::ENOTEMPTY);
            }

            parent_map.remove(name);
            self.write_directory_content(parent, &parent_map);

            let _ = fs::remove_file(self.inode_path(child_ino));
            let _ = fs::remove_file(self.content_path(child_ino));

            if let Ok(mut parent_attr) = self.get_inode(parent) {
                parent_attr.mtime = time_now();
                parent_attr.size = parent_map.len() as u64;
                self.write_inode(&parent_attr);
            }
            Ok(())
        } else {
            Err(libc::ENOENT)
        }
    }

    fn rename(&self, _req: &Request, parent: u64, name: &[u8], newparent: u64, newname: &[u8]) -> Result<(), i32> {
        let mut parent_map = self.get_directory_content(parent)?;
        
        // Grab the file we are moving
        let (child_ino, kind) = match parent_map.get(name) {
            Some(&val) => val,
            None => return Err(libc::ENOENT),
        };

        // If we are renaming within the same directory
        if parent == newparent {
            parent_map.remove(name);
            parent_map.insert(newname.to_vec(), (child_ino, kind));
            self.write_directory_content(parent, &parent_map);
            
            if let Ok(mut p_attr) = self.get_inode(parent) {
                p_attr.mtime = time_now();
                p_attr.size = parent_map.len() as u64;
                self.write_inode(&p_attr);
            }
        } else {
            // We are moving the file to a different directory
            parent_map.remove(name);
            self.write_directory_content(parent, &parent_map);
            if let Ok(mut p_attr) = self.get_inode(parent) {
                p_attr.mtime = time_now();
                p_attr.size = parent_map.len() as u64;
                self.write_inode(&p_attr);
            }

            let mut newparent_map = self.get_directory_content(newparent)?;
            newparent_map.insert(newname.to_vec(), (child_ino, kind));
            self.write_directory_content(newparent, &newparent_map);
            if let Ok(mut np_attr) = self.get_inode(newparent) {
                np_attr.mtime = time_now();
                np_attr.size = newparent_map.len() as u64;
                self.write_inode(&np_attr);
            }
        }
        Ok(())
    }

    fn symlink(&self, _req: &Request, parent: u64, name: &[u8], target: &[u8]) -> Result<FuseEntryOut, i32> {
        let mut parent_attr = self.get_inode(parent)?;
        let ino = self.allocate_next_inode();
        let now = time_now();

        let new_attr = InodeAttributes {
            inode: ino,
            size: target.len() as u64,
            kind: FileKind::Symlink,
            mode: 0o120777, // Symlink permissions
            hardlinks: 1,
            uid: 1000,
            gid: 1000,
            atime: now,
            mtime: now,
            ctime: now,
        };
        self.write_inode(&new_attr);

        // Write the target path bytes into the content backing file
        let mut f = File::create(self.content_path(ino)).map_err(|_| libc::EIO)?;
        f.write_all(target).map_err(|_| libc::EIO)?;

        let mut parent_map = self.get_directory_content(parent)?;
        if parent_map.contains_key(name) { return Err(libc::EEXIST); }
        parent_map.insert(name.to_vec(), (ino, FileKind::Symlink));
        self.write_directory_content(parent, &parent_map);

        parent_attr.mtime = now;
        parent_attr.size = parent_map.len() as u64;
        self.write_inode(&parent_attr);

        Ok(FuseEntryOut {
            nodeid: ino,
            generation: 1,
            entry_valid: 0,
            attr_valid: 0,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr: new_attr.to_fuse_attr(),
        })
    }

    fn readlink(&self, _req: &Request, ino: u64) -> Result<Vec<u8>, i32> {
        let attr = self.get_inode(ino)?;
        if attr.kind != FileKind::Symlink {
            return Err(libc::EINVAL);
        }
        let mut f = File::open(self.content_path(ino)).map_err(|_| libc::ENOENT)?;
        let mut target = Vec::new();
        f.read_to_end(&mut target).map_err(|_| libc::EIO)?;
        Ok(target)
    }

    fn link(&self, _req: &Request, oldnodeid: u64, newparent: u64, newname: &[u8]) -> Result<FuseEntryOut, i32> {
        let mut old_attr = self.get_inode(oldnodeid)?;
        if old_attr.kind == FileKind::Directory {
            return Err(libc::EPERM); // Hardlinking directories is restricted (got to know today :))
        }

        let mut newparent_map = self.get_directory_content(newparent)?;
        if newparent_map.contains_key(newname) {
            return Err(libc::EEXIST);
        }

        old_attr.hardlinks += 1;
        self.write_inode(&old_attr);

        newparent_map.insert(newname.to_vec(), (oldnodeid, old_attr.kind));
        self.write_directory_content(newparent, &newparent_map);

        Ok(FuseEntryOut {
            nodeid: oldnodeid,
            generation: 1,
            entry_valid: 0,
            attr_valid: 0,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr: old_attr.to_fuse_attr(),
        })
    }

    fn statfs(&self, _req: &Request, _ino: u64) -> Result<FuseStatfsOut, i32> {
        Ok(FuseStatfsOut {
            st: FuseKStatfs {
                blocks: 1_000_000,    // Total data blocks (~4GB)
                bfree: 900_000,       // Free blocks
                bavail: 900_000,      // Free blocks for non-root users
                files: 10_000,        // Total available inodes
                ffree: 9_900,         // Free inodes
                bsize: 4096,          // Optimal transfer block size
                namelen: 255,         // Maximum length of filenames
                frsize: 4096,         // Fragment size
                padding: 0,
                spare: [0; 6],
            }
        })
    }

    fn access(&self, req: &Request, ino: u64, mask: u32) -> Result<(), i32> {
        // Fetch the inode (this also implicitly handles F_OK / existence checks)
        let attr = self.get_inode(ino)?;

        // If the mask is 0 (F_OK), the kernel is just asking "Does this exist?"
        if mask == 0 {
            return Ok(());
        }

        // The root user bypasses all permission checks
        if req.uid == 0 {
            return Ok(());
        }

        // Determine which octal bits apply to this request
        let perm_bits = if req.uid == attr.uid {
            (attr.mode >> 6) & 0o7 // Owner bits
        } else if req.gid == attr.gid {
            (attr.mode >> 3) & 0o7 // Group bits
        } else {
            attr.mode & 0o7 // Other bits
        };

        // If the requested mask bits are fully satisfied by the extracted permission bits
        if (mask & perm_bits as u32) == mask {
            Ok(())
        } else {
            Err(libc::EACCES) // Permission denied
        }
    }
}

fn time_now() -> (i64, u32) {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    (t.as_secs() as i64, t.subsec_nanos())
}

fn main() {
    // Initialize the standard formatting subscriber
    tracing_subscriber::fmt::init();

    info!("Starting SimpleFS...");

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: cargo run --example simple <mountpoint> [data_dir]");
        return;
    }
    let mountpoint = &args[1];
    let data_dir_string = if args.len() > 2 {
        args[2].clone()
    } else {
        "/tmp/fuser_data_test1".to_string()
    };

    let data_dir = data_dir_string.as_str();
    println!("mountpoint SimpleFS on {} (Data: {})", mountpoint, data_dir);

    let fs = SimpleFS::new(data_dir);

    if let Err(e) = fuser_iouring::mount(fs, mountpoint, &[MountOption::AutoUnmount]) {
        eprintln!("Error: {}", e);
    }
}