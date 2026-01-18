use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::iter::Fuse;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use fuser_iouring::ll::fuse_abi::*;
use fuser_iouring::{FileSystem , MountOption};

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
                size : 0,
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
            Ok(f) => Ok(bincode::deserialize_from(f).unwrap()),
            Err(_) => Err(libc::ENOENT),
        }
    }

    fn write_directory_content(&self, ino: u64, map: &BTreeMap<Vec<u8>, (u64, FileKind)>) {
        let f = File::create(self.content_path(ino)).unwrap();
        bincode::serialize_into(f, map).unwrap();
    }
}

impl FileSystem for SimpleFS {
    fn lookup(&self, parent: u64, name: &[u8]) -> Result<FuseEntryOut , i32> {
        let dir = self.get_directory_content(parent)?;
        if let Some((ino, _)) = dir.get(name) {
            let attr = self.get_inode(*ino)?;
            Ok(FuseEntryOut{
                nodeid : attr.inode,
                generation : 1,
                entry_valid: 1,
                attr_valid : 1,
                entry_valid_nsec: 0,
                attr_valid_nsec: 0,
                attr: attr.to_fuse_attr(),
            })
        } else {
            Err(libc::ENOENT)
        }
    }

    fn getattr(&self, ino: u64) -> Result<FuseAttrOut, i32> {
        let attr = self.get_inode(ino)?;
        Ok(FuseAttrOut {
            attr_valid: 1,
            attr_valid_nsec: 0,
            dummy: 0,
            attr: attr.to_fuse_attr(),
        })
    }

    fn mkdir(&self, parent: u64, name: &[u8], mode: u32) -> Result<FuseEntryOut, i32> {
        let mut parent_attr = self.get_inode(parent)?;
        if parent_attr.kind != FileKind::Directory { return Err(libc::ENOTDIR); }

        let ino = self.allocate_next_inode();
        let now = time_now();

        let new_attr = InodeAttributes {
            inode: ino,
            size : 0,
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
        self.write_inode(&parent_attr);

        Ok(FuseEntryOut {
            nodeid: ino,
            generation: 1,
            entry_valid: 1,
            attr_valid: 1,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr: new_attr.to_fuse_attr(),
        })
    }

    fn create(&self, parent: u64, name: &[u8], mode: u32) -> Result<(FuseEntryOut, FuseOpenOut), i32> {
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
        self.write_inode(&parent_attr);

        Ok((
            FuseEntryOut{
                nodeid : ino,
                generation: 1,
                entry_valid : 1,
                attr_valid : 1,
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

    fn write(&self, ino:u64 , offset:u64, data: &[u8]) -> Result<u32 , i32> {
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

    fn read(&self, ino:u64 , offset:u64, size: u32) -> Result<Vec<u8> , i32> {
        let attr = self.get_inode(ino)?;
        let mut f = File::open(self.content_path(ino)).map_err(|_| libc::ENOENT)?;

        if offset >= attr.size { return Ok(Vec::new()); }
        f.seek(SeekFrom::Start(offset)).map_err(|_| libc::EIO)?;

        let mut buf = vec![0u8; size as usize];
        let n = f.read(&mut buf).map_err(|_| libc::EIO)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn readdir(&self, ino:u64, offset: u64) -> Result<Vec<u8> , i32> {
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

    fn readdirplus(&self, ino: u64 , offset: u64) -> Result<Vec<u8> , i32> {
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
                entry_valid: 1,
                attr_valid: 1,
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
}

fn time_now() -> (i64, u32) {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    (t.as_secs() as i64, t.subsec_nanos())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: cargo run --example simple <mountpoint>");
        return;
    }
    let mountpoint = &args[1];
    let data_dir = "/tmp/fuser_data_stable3";

    println!("mountpoint SimpleFS on {} (Data : {})", mountpoint, data_dir);

    let fs = SimpleFS::new(data_dir);

    if let Err(e) = fuser_iouring::mount(fs, mountpoint, &[MountOption::AutoUnmount]) {
        eprintln!("Error : {}", e);
    }
}