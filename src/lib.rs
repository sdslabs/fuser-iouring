pub mod ll {
    pub mod fuse_abi;
}
mod mount;
mod transport;

use std::io;
use std::path::Path;
use crate::ll::fuse_abi::*;

pub use mount::MountOption;

pub trait FileSystem {
    fn init(&self, req: &FuseInitIn) -> Result<FuseInitOut, i32> {
        let flags = req.flags & (FUSE_BIG_WRITES | FUSE_ASYNC_READ);
        Ok(FuseInitOut{
            major: 7 , minor: 32, max_readahead: req.max_readahead, flags: flags, max_background: 0, congestion_threshold: 0,
            max_write: 1024*1024, time_gran: 1, padding: [0; 9]
        })
    }

    fn lookup(&self , parent: u64 , name: &[u8]) -> Result<FuseEntryOut, i32> { Err(libc::ENOSYS) }
    fn getattr(&self, ino: u64) -> Result<FuseAttrOut, i32> { Err(libc::ENOSYS) }
    fn mkdir(&self, parent: u64, name: &[u8], mode: u32) -> Result<FuseEntryOut, i32> { Err(libc::ENOSYS) }
    fn create(&self, parent:u64, name: &[u8], mode: u32 ) -> Result<(FuseEntryOut, FuseOpenOut), i32> { Err(libc::ENOSYS) }
    fn write(&self, ino:u64, offset: u64, data: &[u8]) -> Result<u32, i32> { Err(libc::ENOSYS) }
    fn read(&self, ino:u64, offset: u64, size: u32) -> Result<Vec<u8>, i32> { Err(libc::ENOSYS) }
    fn readdir(&self, ino:u64, offset: u64) -> Result<Vec<u8>, i32> { Err(libc::ENOSYS) }

    fn access(&self , _ino:u64, _mask:u32) -> Result<() , i32> { Ok(()) }
    fn open(&self, _ino:u64, _flags:u32) -> Result<FuseOpenOut, i32> {
        Ok(FuseOpenOut { fh:0, open_flags:0, padding: 0 })
    }
    fn opendir(&self, _ino:u64, _flags:u32) -> Result<FuseOpenOut, i32> {
        Ok(FuseOpenOut { fh:0, open_flags:0, padding: 0 })
    }
    fn readdirplus(&self, ino: u64, offset: u64) -> Result<Vec<u8> , i32> {
        Err(libc::ENOSYS)
    }
}

pub fn mount<F, P>(fs: F, mountpoint: P, options: &[MountOption]) -> io::Result<()>
where
    F: FileSystem + Send + 'static,
    P: AsRef<Path>,
{
    let session = mount::Session::new(mountpoint.as_ref(), options)?;

    // Start the io_uring loop (ignoring `fs` (for now only))
    transport::run_uring_loop(session.fd , fs)?;

    Ok(())
}
