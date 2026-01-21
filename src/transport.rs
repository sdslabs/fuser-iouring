use crate::ll::fuse_abi::*;
use io_uring::{IoUring, opcode, types};
use std::io;
use std::mem;
use std::os::unix::io::RawFd;
use std::slice;
use std::ffi::CStr;
use crate::FileSystem;

const MAX_WRITE_SIZE: u32 = 1024 * 1024;
const FUSE_BUFFER_SIZE: usize = (MAX_WRITE_SIZE as usize) + 4096;

pub fn run_uring_loop<F : FileSystem>(fuse_fd: RawFd , fs: F) -> io::Result<()> {
    let mut ring = IoUring::new(8)?;
    let mut buf = vec![0u8; FUSE_BUFFER_SIZE];

    let read_op = opcode::Read::new(types::Fd(fuse_fd), buf.as_mut_ptr(), buf.len() as _)
        .offset(-1i64 as u64)
        .build()
        .user_data(0x01);

    unsafe {
        ring.submission()
            .push(&read_op)
            .expect("submission queue full");
    }
    ring.submit()?;

    println!("Entered io_uring event loop...");

    loop {
        ring.submit_and_wait(1)?;

        let cqe = ring.completion().next().expect("completion queue empty");

        if cqe.user_data() == 0x01 {
            if cqe.result() <= 0 {
                eprintln!("Read error from FUSE device: {}", cqe.result());
                break;
            }

            let bytes_read = cqe.result() as usize;
            if bytes_read >= mem::size_of::<FuseInHeader>() {
                let header = unsafe { &*(buf.as_ptr() as *const FuseInHeader) };
                // For now , letting it here for debugging purpose
                // println!("Header: {:?}", header);

                let res = match header.opcode {
                    FUSE_INIT => {
                        // println!("test init");
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseInitIn) };
                        match fs.init(&arg) {
                            Ok(entry) => reply_ok(&mut ring, fuse_fd, header.unique, &entry),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_LOOKUP => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let name = unsafe { CStr::from_ptr(ptr as *const i8) };
                        match fs.lookup(header.nodeid , name.to_bytes()) {
                            Ok(entry) => reply_ok(&mut ring, fuse_fd, header.unique, &entry),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_GETATTR => {
                        match fs.getattr(header.nodeid ) {
                            Ok(out) => reply_ok(&mut ring, fuse_fd, header.unique, &out),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_ACCESS => {
                        match fs.access(header.nodeid , 0) {
                            Ok(_) => reply_ok(&mut ring, fuse_fd, header.unique, &()),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_OPEN => {
                        match fs.open(header.nodeid, 0) {
                            Ok(out) => reply_ok(&mut ring, fuse_fd, header.unique, &out),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_OPENDIR => {
                        // println!("opendir match cased");
                        match fs.opendir(header.nodeid, 0) {
                            Ok(out) => reply_ok(&mut ring, fuse_fd, header.unique, &out),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_MKDIR => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseMkdirIn) };
                        let name_ptr = unsafe { ptr.add(mem::size_of::<FuseMkdirIn>()) };
                        let name = unsafe { CStr::from_ptr(name_ptr as *const i8) };

                        match fs.mkdir(header.nodeid , name.to_bytes(), arg.mode) {
                            Ok(entry) => reply_ok(&mut ring, fuse_fd, header.unique, &entry),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_CREATE => {
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseCreateIn) };
                        let name_ptr = unsafe { ptr.add(mem::size_of::<FuseCreateIn>()) };
                        let name = unsafe { CStr::from_ptr(name_ptr as *const i8) };

                        match fs.create(header.nodeid , name.to_bytes(), arg.mode) {
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

                        match fs.write(header.nodeid, arg.offset, data) {
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
                        match fs.read(header.nodeid, arg.offset, arg.size) {
                            Ok(data) => reply_data(&mut ring, fuse_fd, header.unique, &data),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_READDIR => {
                        println!("read request");
                        let ptr = unsafe { buf.as_ptr().add(mem::size_of::<FuseInHeader>()) };
                        let arg = unsafe { *(ptr as *const FuseReadIn) };

                        match fs.readdir(header.nodeid , arg.offset) {
                            Ok(data) => reply_data(&mut ring, fuse_fd, header.unique, &data),
                            Err(e) => reply_error(&mut ring, fuse_fd, header.unique, e),
                        }
                    },
                    FUSE_READDIRPLUS => {
                        reply_error(&mut ring , fuse_fd , header.unique, libc::ENOSYS)
                    },
                    _ => reply_error(&mut ring, fuse_fd, header.unique, libc::ENOSYS),
                };
                if let Err(e) = res { eprintln!("IO error: {}", e); }
            }
        }

        unsafe {
            ring.submission()
                .push(&read_op)
                .expect("submission queue full");
        }
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