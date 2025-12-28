pub mod ll {
    pub mod fuse_abi;
}
mod mount;
mod transport;

use std::io;
use std::path::Path;

pub use mount::MountOption;

pub trait FileSystem {}

pub fn mount<F, P>(_fs: F, mountpoint: P, options: &[MountOption]) -> io::Result<()>
where
    F: FileSystem + Send + 'static,
    P: AsRef<Path>,
{
    let session = mount::Session::new(mountpoint.as_ref(), options)?;

    // Start the io_uring loop (ignoring `fs` (for now only))
    transport::run_uring_loop(session.fd)?;

    Ok(())
}
