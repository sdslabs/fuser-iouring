use libc::{c_int, c_void};
use std::io::{self, Error};
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{mem, ptr};

#[derive(Debug, Clone)]
pub enum MountOption {
    AutoUnmount,
    AllowOther,
    RO,
}

pub struct Session {
    pub fd: RawFd,
    mountpoint: PathBuf,
}

impl Session {
    pub fn new(mountpoint: &Path, options: &[MountOption]) -> io::Result<Self> {
        let mountpoint = mountpoint.canonicalize()?;
        let mut fuse_opts = Vec::new();
        for opt in options {
            match opt {
                MountOption::AutoUnmount => fuse_opts.push("auto_unmount"),
                MountOption::AllowOther => fuse_opts.push("allow_other"),
                MountOption::RO => fuse_opts.push("ro"),
            }
        }
        let opts_string = fuse_opts.join(",");
        let fd = fuse_mount_sys(&mountpoint, &opts_string)?;
        Ok(Session { fd, mountpoint })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
        let _ = Command::new("fusermount")
            .arg("-u")
            .arg("-q")
            .arg("-z")
            .arg(&self.mountpoint)
            .status();
    }
}

fn fuse_mount_sys(mountpoint: &Path, options: &str) -> io::Result<RawFd> {
    let (sock0, sock1) = socket_pair()?;

    let res = unsafe { libc::fcntl(sock0, libc::F_SETFD, 0) };
    if res < 0 {
        return Err(Error::last_os_error());
    }

    let _child = Command::new("fusermount")
        .arg("-o")
        .arg(options)
        .arg("--")
        .arg(mountpoint)
        .env("_FUSE_COMMFD", sock0.to_string())
        .spawn()?;

    unsafe { libc::close(sock0) };

    let fuse_fd_res = receive_fd(sock1);

    unsafe { libc::close(sock1) };

    fuse_fd_res
}

fn socket_pair() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0 as c_int; 2];
    let res = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    if res < 0 {
        return Err(Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

fn receive_fd(socket: RawFd) -> io::Result<RawFd> {
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    let mut iov: libc::iovec = unsafe { mem::zeroed() };

    let mut dummy_buf = [0u8; 1];
    iov.iov_base = dummy_buf.as_mut_ptr() as *mut c_void;
    iov.iov_len = dummy_buf.len();

    #[repr(C)]
    union AlignedBuffer {
        buf: [u8; 128],
        align: libc::cmsghdr,
    }
    let mut cmsg_storage = AlignedBuffer { buf: [0u8; 128] };

    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = unsafe { cmsg_storage.buf.as_mut_ptr() as *mut c_void };
    msg.msg_controllen = unsafe { cmsg_storage.buf.len() as _ };

    let res = unsafe { libc::recvmsg(socket, &mut msg, 0) };
    if res < 0 {
        return Err(Error::last_os_error());
    }

    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg.is_null() {
        if unsafe {
            (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS
        } {
            let data_ptr = unsafe { libc::CMSG_DATA(cmsg) };
            let mut fd: c_int = 0;

            unsafe {
                ptr::copy_nonoverlapping(
                    data_ptr,
                    &mut fd as *mut c_int as *mut u8,
                    mem::size_of::<c_int>(),
                );
            }

            return Ok(fd);
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
    }

    Err(Error::other("No file descriptor received from fusermount"))
}
