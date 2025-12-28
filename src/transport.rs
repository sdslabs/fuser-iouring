use crate::ll::fuse_abi::*;
use io_uring::{IoUring, opcode, types};
use std::io;
use std::mem;
use std::os::unix::io::RawFd;
use std::slice;

const FUSE_BUFFER_SIZE: usize = 1024 * 128; // 128kb

pub fn run_uring_loop(fuse_fd: RawFd) -> io::Result<()> {
    let mut ring = IoUring::new(8)?;
    let mut buf = vec![0u8; FUSE_BUFFER_SIZE];

    let read_op = opcode::Read::new(types::Fd(fuse_fd), buf.as_mut_ptr(), buf.len() as _)
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
            if cqe.result() < 0 {
                eprintln!("Read error from FUSE device: {}", cqe.result());
                break;
            }

            let bytes_read = cqe.result() as usize;

            unsafe {
                ring.submission()
                    .push(&read_op)
                    .expect("submission queue full");
            }
            ring.submit()?;

            if bytes_read >= mem::size_of::<fuse_in_header>() {
                let header = unsafe { &*(buf.as_ptr() as *const fuse_in_header) };

                if header.opcode == FUSE_INIT {
                    handle_init(&mut ring, fuse_fd, header, &buf)?;
                } else {
                    println!(
                        "Ignored Opcode: {} (Unique: {})",
                        header.opcode, header.unique
                    );
                    reply_error(&mut ring, fuse_fd, header.unique, libc::ENOSYS)?;
                }
            }
        }
    }
    Ok(())
}

fn handle_init(
    ring: &mut IoUring,
    fd: RawFd,
    header: &fuse_in_header,
    buf: &[u8],
) -> io::Result<()> {
    let data_ptr = unsafe { buf.as_ptr().add(mem::size_of::<fuse_in_header>()) };
    let in_args = unsafe { *(data_ptr as *const fuse_init_in) };

    println!("Received INIT: Kernel v{}.{}", in_args.major, in_args.minor);

    let out_header = fuse_out_header {
        len: (mem::size_of::<fuse_out_header>() + mem::size_of::<fuse_init_out>()) as u32,
        error: 0,
        unique: header.unique,
    };

    let out_args = fuse_init_out {
        major: 7,
        minor: 31,
        max_readahead: in_args.max_readahead,
        flags: in_args.flags,
        max_background: 0,
        congestion_threshold: 0,
        max_write: 1024 * 1024,
        time_gran: 1,
        padding: [0; 9],
    };

    let mut reply_vec = Vec::new();
    let header_bytes = unsafe {
        slice::from_raw_parts(
            &out_header as *const _ as *const u8,
            mem::size_of::<fuse_out_header>(),
        )
    };
    let args_bytes = unsafe {
        slice::from_raw_parts(
            &out_args as *const _ as *const u8,
            mem::size_of::<fuse_init_out>(),
        )
    };

    reply_vec.extend_from_slice(header_bytes);
    reply_vec.extend_from_slice(args_bytes);

    let write_op = opcode::Write::new(types::Fd(fd), reply_vec.as_ptr(), reply_vec.len() as _)
        .build()
        .user_data(0x02);

    unsafe {
        ring.submission().push(&write_op).expect("sq full");
    }

    ring.submit_and_wait(1)?;

    let _ = ring.completion().next();
    println!("Replied to INIT (initialization) successfully.");

    Ok(())
}

fn reply_error(ring: &mut IoUring, fd: RawFd, unique: u64, error_code: i32) -> io::Result<()> {
    let header = fuse_out_header {
        len: mem::size_of::<fuse_out_header>() as u32,
        error: -error_code, // e.g. -38 for ENOSYS
        unique,
    };

    let buf = unsafe {
        slice::from_raw_parts(
            &header as *const _ as *const u8,
            mem::size_of::<fuse_out_header>(),
        )
    };

    let write_op = opcode::Write::new(types::Fd(fd), buf.as_ptr(), buf.len() as _)
        .build()
        .user_data(0x03);

    unsafe {
        ring.submission().push(&write_op).expect("sq full");
    }
    ring.submit_and_wait(1)?;
    ring.completion().next();

    Ok(())
}
