use crate::ll::fuse_abi::*;
use io_uring::{IoUring, opcode, types};
use std::io;
use std::mem;
use std::os::unix::io::RawFd;
use std::slice;
use std::ffi::CStr;
use crate::{FileSystem, Request};
use tracing::{info, error};

const MAX_WRITE_SIZE: u32 = 1024 * 1024;
const FUSE_BUFFER_SIZE: usize = (MAX_WRITE_SIZE as usize) + 4096;

pub fn run_uring_loop<F : FileSystem>(fuse_fd: RawFd , fs: F) -> io::Result<()> {
    let mut ring = IoUring::new(8)?;
    let mut buf = vec![0u8; FUSE_BUFFER_SIZE];

    let read_op = opcode::Read::new(types::Fd(fuse_fd), buf.as_mut_ptr(), buf.len() as _)
        .offset(-1i64 as u64)
        .build()
        .user_data(0x01);

    unsafe { ring.submission().push(&read_op).expect("submission queue full"); }
    ring.submit()?;
    // println!("Entered io_uring event loop...");

    info!("Entered io_uring event loop...");

    loop {
        ring.submit_and_wait(1)?;
        let cqe = ring.completion().next().expect("completion queue empty");

        if cqe.user_data() == 0x01 {
            if cqe.result() <= 0 {
                let res = cqe.result();
                // -19 (-ENODEV) or 0 means the FUSE connection was unmounted gracefully
                if res == -libc::ENODEV || res == 0 {
                    // println!("FUSE session terminated (unmounted gracefully).");
                    info!("FUSE session terminated (unmounted gracefully).");
                } else {
                    // eprintln!("Read error from FUSE device: {}", res);
                    error!("Read error from FUSE device: {}", res);
                }
                break;
            }

            let bytes_read = cqe.result() as usize;
            if bytes_read >= mem::size_of::<FuseInHeader>() {
                let header = unsafe { &*(buf.as_ptr() as *const FuseInHeader) };
                
                // CREATE THE REQUEST CONTEXT
                    let req = Request {
                        unique: header.unique,
                        uid: header.uid,
                        gid: header.gid,
                        pid: header.pid,
                    };

                let res = match header.opcode {
                    FUSE_INIT => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseInitIn) };
                        match fs.init(&req, &arg) {
                            Ok(entry) => reply_ok(&mut ring, fuse_fd, header.unique, &entry),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_LOOKUP => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let name = unsafe { CStr::from_ptr(ptr as *const i8) };
                        match fs.lookup(&req, header.nodeid, name.to_bytes()) {
                            Ok(entry) => reply_ok(&mut ring, fuse_fd, header.unique, &entry),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_GETATTR => {
                        match fs.getattr(&req, header.nodeid) {
                            Ok(out) => reply_ok(&mut ring, fuse_fd, header.unique, &out),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_SETATTR => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseSetAttrIn) };
                        match fs.setattr(&req, header.nodeid, &arg) {
                            Ok(out) => reply_ok(&mut ring, fuse_fd, header.unique, &out),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_ACCESS => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseAccessIn) };
                        match fs.access(&req, header.nodeid, arg.mask) {
                            Ok(_) => reply_ok(&mut ring, fuse_fd, header.unique, &()),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_OPEN => {
                        match fs.open(&req, header.nodeid, 0) {
                            Ok(out) => reply_ok(&mut ring, fuse_fd, header.unique, &out),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_OPENDIR => {
                        match fs.opendir(&req, header.nodeid, 0) {
                            Ok(out) => reply_ok(&mut ring, fuse_fd, header.unique, &out),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_MKDIR => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseMkdirIn) };
                        let name_ptr = unsafe { ptr.add(mem::size_of::<FuseMkdirIn>()) };
                        let name = unsafe { CStr::from_ptr(name_ptr as *const i8) };
                        match fs.mkdir(&req, header.nodeid, name.to_bytes(), arg.mode) {
                            Ok(entry) => reply_ok(&mut ring, fuse_fd, header.unique, &entry),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_CREATE => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseCreateIn) };
                        let name_ptr = unsafe { ptr.add(mem::size_of::<FuseCreateIn>()) };
                        let name = unsafe { CStr::from_ptr(name_ptr as *const i8) };
                        match fs.create(&req, header.nodeid, name.to_bytes(), arg.mode) {
                            Ok((entry , open)) => {
                                let out = FuseCreateOut {entry , open};
                                reply_ok(&mut ring, fuse_fd, header.unique, &out)
                            },
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_WRITE => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseWriteIn) };
                        let data_ptr = unsafe { ptr.add(mem::size_of::<FuseWriteIn>()) };
                        let data = unsafe { slice::from_raw_parts(data_ptr , arg.size as usize) };
                        match fs.write(&req, header.nodeid, arg.offset, data) {
                            Ok(written) => {
                                let out = FuseWriteOut { size: written, padding: 0 };
                                reply_ok(&mut ring, fuse_fd, header.unique, &out)
                            }
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_READ => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseReadIn) };
                        match fs.read(&req, header.nodeid, arg.offset, arg.size) {
                            Ok(data) => reply_data(&mut ring, fuse_fd, header.unique, &data),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_READDIR => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseReadIn) };
                        match fs.readdir(&req, header.nodeid, arg.offset) {
                            Ok(data) => reply_data(&mut ring, fuse_fd, header.unique, &data),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_READDIRPLUS => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseReadIn) };
                        match fs.readdirplus(&req, header.nodeid, arg.offset) {
                            Ok(data) => reply_data(&mut ring, fuse_fd, header.unique, &data),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_UNLINK => {
                        let ptr = unsafe{ buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let name = unsafe{ CStr::from_ptr(ptr as *const i8) };
                        match fs.unlink(&req, header.nodeid, name.to_bytes()) {
                            Ok(()) => reply_ok(&mut ring, fuse_fd, header.unique, &()),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_RMDIR => {
                        let ptr = unsafe{ buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let name = unsafe{ CStr::from_ptr(ptr as *const i8) };
                        match fs.rmdir(&req, header.nodeid, name.to_bytes()) {
                            Ok(()) => reply_ok(&mut ring, fuse_fd, header.unique, &()),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_RENAME => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseRenameIn) };
                        let name_ptr = unsafe { ptr.add(mem::size_of::<FuseRenameIn>()) };
                        let oldname = unsafe { CStr::from_ptr(name_ptr as *const i8) };
                        // The new name starts right after the null terminator of the old name
                        let newname_ptr = unsafe { name_ptr.add(oldname.to_bytes_with_nul().len()) };
                        let newname = unsafe { CStr::from_ptr(newname_ptr as *const i8) };

                        match fs.rename(&req, header.nodeid, oldname.to_bytes(), arg.newdir, newname.to_bytes()) {
                            Ok(()) => reply_ok(&mut ring, fuse_fd, header.unique, &()),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_SYMLINK => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>())};
                        let name = unsafe { CStr::from_ptr(ptr as *const i8) };
                        let target_ptr = unsafe { ptr.add(name.to_bytes_with_nul().len())};
                        let target = unsafe { CStr::from_ptr(target_ptr as *const i8) };
                        match fs.symlink(&req, header.nodeid, name.to_bytes(), target.to_bytes()) {
                            Ok(entry) => reply_ok(&mut ring, fuse_fd, header.unique, &entry),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_READLINK => {
                        match fs.readlink(&req, header.nodeid) {
                            Ok(target) => reply_data(&mut ring, fuse_fd, header.unique, &target),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_LINK => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseLinkIn) };
                        let name_ptr = unsafe { ptr.add(mem::size_of::<FuseLinkIn>()) };
                        let name = unsafe { CStr::from_ptr(name_ptr as *const i8) };
                        match fs.link(&req, arg.oldnodeid, header.nodeid, name.to_bytes()) {
                            Ok(entry) => reply_ok(&mut ring, fuse_fd, header.unique, &entry),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_STATFS => {
                        match fs.statfs(&req, header.nodeid) {
                            Ok(out) => reply_ok(&mut ring, fuse_fd, header.unique, &out),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    _ => reply_error(&mut ring, fuse_fd, header.unique, libc::ENOSYS),
                };
                if let Err(e) = res { eprintln!("IO error: {}", e); }
            }
        }

        unsafe { ring.submission().push(&read_op).expect("submission queue full"); }
        ring.submit()?;
    }
    Ok(())
}

fn reply_ok<T: Sized>(ring: &mut IoUring, fd: RawFd, unique: u64, val:&T) -> io::Result<()> {
    if mem::size_of::<T>() == 0 {
        return reply_data(ring, fd, unique, &[]);
    }
    let slice = unsafe { slice::from_raw_parts(val as *const T as *const u8, mem::size_of::<T>())};
    reply_data(ring , fd , unique, slice)
}

fn reply_data(ring: &mut IoUring, fd: RawFd, unique: u64, data: &[u8]) -> io::Result<()> {
    let out = FuseOutHeader {len : (mem::size_of::<FuseOutHeader>() + data.len()) as u32 , error: 0 ,unique};
    let hdr_slice = unsafe { slice::from_raw_parts(&out as *const _ as *const u8, mem::size_of::<FuseOutHeader>())};
    let mut vec = Vec::with_capacity(hdr_slice.len() + data.len());
    vec.extend_from_slice(hdr_slice);
    vec.extend_from_slice(data);
    let op = opcode::Write::new(types::Fd(fd), vec.as_ptr(), vec.len() as _)
        .offset(-1i64 as u64)
        .build().user_data(0x02);
    unsafe{ ring.submission().push(&op).expect("submission queue full"); }
    loop {
        ring.submit_and_wait(1)?;
        if let Some(cqe) = ring.completion().next() {
            if cqe.user_data() == 0x02 { break; }
        }
    }
    Ok(())
}

fn reply_error(ring: &mut IoUring, fd: RawFd, unique: u64, err: i32) -> io::Result<()> {
    let out = FuseOutHeader { len: mem::size_of::<FuseOutHeader>() as u32, error: -err, unique };
    let slice = unsafe { slice::from_raw_parts(&out as *const _ as *const u8, mem::size_of::<FuseOutHeader>()) };
    let op = opcode::Write::new(types::Fd(fd), slice.as_ptr(), slice.len() as _)
        .offset(-1i64 as u64)
        .build().user_data(0x03);
    unsafe { ring.submission().push(&op).expect("sq full"); }
    loop {
        ring.submit_and_wait(1)?;
        if let Some(cqe) = ring.completion().next() {
            if cqe.user_data() == 0x03 { break; }
        }
    }
    Ok(())
}