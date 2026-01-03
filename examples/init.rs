use fuser_iouring::{FileSystem, MountOption};
use std::env;

struct NullFS;

impl FileSystem for NullFS {}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("missing arguments: please provide a mountpoint");
        return;
    }
    let mountpoint = &args[1];

    let options = vec![MountOption::AutoUnmount];

    println!("Mounting NullFS on {}", mountpoint);

    match fuser_iouring::mount(NullFS, mountpoint, &options) {
        Ok(_) => println!("Mount mounted successfully"),
        Err(e) => println!("Error in initialising FUSE session: {}", e),
    }
}
