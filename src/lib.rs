pub mod ll {
    pub mod fuse_abi;
}
mod mount;
mod transport;

use std::io;
use std::path::Path;
use crate::ll::fuse_abi::*;

pub use mount::MountOption;

#[derive(Debug, Clone, Copy)]
pub struct Request {
    pub unique: u64,
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
}

pub trait FileSystem {
    fn init(&self, _req: &Request, req: &FuseInitIn) -> Result<FuseInitOut, i32> {
        let flags = req.flags & (FUSE_BIG_WRITES | FUSE_ASYNC_READ);
        Ok(FuseInitOut{
            major: 7 , minor: 18, max_readahead: req.max_readahead, flags: flags, max_background: 0, congestion_threshold: 0,
            max_write: 1024*1024, time_gran: 1, padding: [0; 9]
        })
    }

    fn lookup(&self , _req: &Request, _parent: u64 , _name: &[u8]) -> Result<FuseEntryOut, i32> { Err(libc::ENOSYS) }
    fn getattr(&self, _req: &Request, _ino: u64) -> Result<FuseAttrOut, i32> { Err(libc::ENOSYS) }
    fn setattr(&self, _req: &Request, _ino: u64, _arg: &FuseSetAttrIn) -> Result<FuseAttrOut, i32> { Err(libc::ENOSYS) }
    fn mkdir(&self, _req: &Request, _parent: u64, _name: &[u8], _mode: u32) -> Result<FuseEntryOut, i32> { Err(libc::ENOSYS) }
    fn create(&self, _req: &Request, _parent: u64, _name: &[u8], _mode: u32 ) -> Result<(FuseEntryOut, FuseOpenOut), i32> { Err(libc::ENOSYS) }
    fn write(&self, _req: &Request, _ino: u64, _offset: u64, _data: &[u8]) -> Result<u32, i32> { Err(libc::ENOSYS) }
    fn read(&self, _req: &Request, _ino: u64, _offset: u64, _size: u32) -> Result<Vec<u8>, i32> { Err(libc::ENOSYS) }
    fn readdir(&self, _req: &Request, _ino: u64, _offset: u64) -> Result<Vec<u8>, i32> { Err(libc::ENOSYS) }
    fn readdirplus(&self, _req: &Request, _ino: u64, _offset: u64) -> Result<Vec<u8> , i32> { Err(libc::ENOSYS) }
    
    fn access(&self , _req: &Request, _ino: u64, _mask: u32) -> Result<() , i32> { Ok(()) }
    fn open(&self, _req: &Request, _ino: u64, _flags: u32) -> Result<FuseOpenOut, i32> {
        Ok(FuseOpenOut { fh:0, open_flags:0, padding: 0 })
    }
    fn opendir(&self, _req: &Request, _ino: u64, _flags: u32) -> Result<FuseOpenOut, i32> {
        Ok(FuseOpenOut { fh:0, open_flags:0, padding: 0 })
    }
    
    fn unlink(&self, _req: &Request, _parent: u64, _name: &[u8]) -> Result<(), i32> { Err(libc::ENOSYS) }
    fn rmdir(&self, _req: &Request, _parent: u64, _name: &[u8]) -> Result<(), i32> { Err(libc::ENOSYS) }
    fn rename(&self, _req: &Request, _parent: u64, _name: &[u8], _newparent: u64, _newname: &[u8]) -> Result<(), i32> { Err(libc::ENOSYS) }

    fn symlink(&self, _req: &Request, _parent: u64, _name: &[u8], _target: &[u8]) -> Result<FuseEntryOut, i32> {Err(libc::ENOSYS) }
    fn readlink(&self, _req: &Request, _ino: u64) -> Result<Vec<u8>, i32> {Err(libc::ENOSYS) }
    fn link(&self, _req: &Request, _ino: u64, _newparent: u64, _newname: &[u8]) -> Result<FuseEntryOut, i32> {Err(libc::ENOSYS) }

}

pub fn mount<F, P>(fs: F, mountpoint: P, options: &[MountOption]) -> io::Result<()>
where
    F: FileSystem + Send + 'static,
    P: AsRef<Path>,
{
    let session = mount::Session::new(mountpoint.as_ref(), options)?;

    // Start the io_uring loop
    transport::run_uring_loop(session.fd , fs)?;

    Ok(())
}
