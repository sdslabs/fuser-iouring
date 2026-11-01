# fuser-iouring

[![crates.io](https://img.shields.io/crates/v/fuser-iouring.svg)](https://crates.io/crates/fuser-iouring)
[![docs.rs](https://docs.rs/fuser-iouring/badge.svg)](https://docs.rs/fuser-iouring)
[![License](https://img.shields.io/crates/l/fuser-iouring.svg)](LICENSE)

A Rust library for writing FUSE filesystems, with all communication with the kernel going through `io_uring`.

## Why

Libraries such as [`fuser`](https://github.com/cberner/fuser) talk to the kernel using blocking `read(2)` and `write(2)` calls on `/dev/fuse`. Each request costs at least two syscalls, and the event loop is built around a blocking model.

`fuser-iouring` sends those reads and writes through an `io_uring` submission queue instead. That gives you a foundation for batching, fewer context switches, and moving toward the kernel's native FUSE-over-io_uring transport, without changing how you write a filesystem.

> **Status:** under development. The API is not yet stable, and some operations (xattrs, `fsync`, `release`, `forget`, locks) are not dispatched yet.

## Requirements

- Linux 5.6 or newer (for `io_uring`)
- `fusermount` available on `PATH` (shipped with `fuse` / `fuse3`)

## Quick start

```sh
cargo add fuser-iouring libc
```

or add it to `Cargo.toml` manually:

```toml
[dependencies]
fuser-iouring = "0.1"
libc = "0.2"
```

```rust
use fuser_iouring::{FileSystem, MountOption, Request};
use fuser_iouring::ll::fuse_abi::FuseAttrOut;

struct HelloFs;

impl FileSystem for HelloFs {
    fn getattr(&self, _req: &Request, ino: u64) -> Result<FuseAttrOut, i32> {
        // Fill in attributes for `ino`, or return an errno.
        Err(libc::ENOENT)
    }
}

fn main() -> std::io::Result<()> {
    fuser_iouring::mount(HelloFs, "/mnt/hello", &[MountOption::AutoUnmount])
}
```

`mount` blocks until the filesystem is unmounted.

## API

The API follows [`fuser`](https://docs.rs/fuser) closely, so moving between the two is straightforward:

- Implement the `FileSystem` trait. Each method maps to one FUSE operation (`lookup`, `getattr`, `read`, `write`, `readdir`, and so on), and any method you don't override returns `ENOSYS`.
- Return `Ok(reply)` or `Err(errno)`. Results are returned directly, rather than through `fuser`'s reply objects.
- Call `fuser_iouring::mount(fs, mountpoint, options)` to mount and run the event loop.
- Raw kernel ABI structs and constants live in `fuser_iouring::ll::fuse_abi`.

Full reference: [docs.rs/fuser-iouring](https://docs.rs/fuser-iouring).

## Examples

| Example       | Description                                         |
|---------------|-----------------------------------------------------|
| `init`        | Minimal filesystem that only completes `FUSE_INIT`  |
| `simple`      | In-memory style filesystem persisted to a data dir  |
| `passthrough` | Mirrors an existing directory                       |

```sh
mkdir -p /tmp/mnt /tmp/data
cargo run --example passthrough /tmp/mnt /tmp/data
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
