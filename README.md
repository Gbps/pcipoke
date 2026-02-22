# pcipoke

A small userspace-only Linux command-line tool for reading and writing PCI device BAR (Base Address Register) memory directly using the kernel's sysfs interface.

All accesses are performed with fixed length `mmap` and `volatile` loads/stores which ensures correct operation when spanning alignment.

This tool is useful for working with devices like FPGAs without needing to write a Linux device driver.

Before any operation, `pcipoke` reads the device's PCI Command register via `setpci` and automatically enables **Memory Space** and **Bus Master** bits if they are not already set. This is because Linux kernel will not typically enable command register bits until it believes the device is in use by a driver.

## Requirements

- Linux only
- Run as **root** (required to map device resources to userspace `/sys/bus/pci/devices/*/resource*`)
- [`pciutils`](https://github.com/pciutils/pciutils) installed (`setpci` must be on `$PATH` to enable command register)
- Rust toolchain (to build)

## Building

```sh
cargo build --release
```

The binary will be at `target/release/pcipoke`.

## Usage

```
pcipoke <ADDRESS> <OPERATION> <COUNT|DATA> [OPTIONS]
```

### Arguments

| Argument | Description |
|---|---|
| `ADDRESS` | PCI address in `[DDDD:]BB:DD.F` form. The domain (`DDDD:`) defaults to `0000` if omitted. |
| `OPERATION` | `r` to read, `w` to write. |
| `COUNT\|DATA` | For reads: number of bytes (decimal or `0x` hex). For writes: value to write in hex (e.g. `0xDEADBEEF` or `DEADBEEF`). |

### Options

| Flag | Default | Description |
|---|---|---|
| `-b`, `--bar-num <N>` | `0` | BAR number to access (0–5). 64-bit BARs occupy two consecutive BAR numbers. |
| `-o`, `--offset <OFFSET>` | `0` | Byte offset into the BAR to start from. Supports optional `0x` hex prefix. |
| `-s`, `--read-size <N>` | `4` | Width of each MMIO read in bytes: `1`, `2`, `4`, or `8`. |
| `-w`, `--write-size <N>` | `4` | Width of the MMIO write in bytes: `1`, `2`, or `4`, or `8`. |

## Examples

### Read 64 bytes from BAR 0

```bash
# sudo pcipoke 01:00.0 r 64
READ: 0000:01:00.0 resource0  offset 0x0  length 0x40 (64 bytes)  read-size 4 bytes
OK: Command register 0x0146 — MEM and BUS MASTER already enabled
00000000  01 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00  |................|
00000010  00 00 00 00 ff ff ff ff  00 00 00 00 00 00 00 00  |................|
00000020  00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00  |................|
00000030  00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00  |................|
00000040
```

### Read with a hex byte count and non-zero offset

```bash
# sudo pcipoke 0000:03:00.0 r 0x80 --offset 0x100 --read-size 4
READ: 0000:03:00.0 resource0  offset 0x100  length 0x80 (128 bytes)  read-size 4 bytes
OK: Command register 0x0146 — MEM and BUS MASTER already enabled
00000100  78 56 34 12 00 00 00 00  ff ff ff ff 00 00 00 00  |xV4.............|
00000110  00 00 01 00 00 00 00 00  00 00 00 00 00 00 00 00  |................|
...
00000180
```

> NOTE: Reads larger than `read-size` are aligned to the `read-size` value automatically.

### Read from BAR 2 using 1-byte accesses

```bash
# sudo pcipoke 01:00.0 r 16 --bar-num 2 --read-size 1
READ: 0000:01:00.0 resource2  offset 0x0  length 0x10 (16 bytes)  read-size 1 bytes
OK: Command register 0x0146 — MEM and BUS MASTER already enabled
00000000  de ad be ef ca fe ba be  00 11 22 33 44 55 66 77  |............"3DUfw|
00000010
```

### Write a 32-bit value

```bash
# sudo pcipoke 01:00.0 w 0xDEADBEEF --offset 0x10
WRITE: 0000:01:00.0 resource0  offset 0x10  data 0xdeadbeef  write-size 4 bytes
OK: Command register 0x0146 — MEM and BUS MASTER already enabled
OK: wrote 0xdeadbeef to offset 0x10
```

### Write a 16-bit value

```bash
# sudo pcipoke 01:00.0 w 0xCAFE --offset 0x4 --write-size 2
WRITE: 0000:01:00.0 resource0  offset 0x4  data 0xcafe  write-size 2 bytes
OK: Command register 0x0146 — MEM and BUS MASTER already enabled
OK: wrote 0xcafe to offset 0x4
```

### Write a single byte

```bash
# sudo pcipoke 01:00.0 w FF --offset 0x8 --write-size 1
WRITE: 0000:01:00.0 resource0  offset 0x8  data 0xff  write-size 1 bytes
OK: Command register 0x0146 — MEM and BUS MASTER already enabled
OK: wrote 0xff to offset 0x8
```

### Automatically enabling a disabled device

If the device's Memory Space or Bus Master bits are not set, `pcipoke` enables them automatically before accessing the BAR:

```bash
# sudo pcipoke 02:00.0 r 32
READ: 0000:02:00.0 resource0  offset 0x0  length 0x20 (32 bytes)  read-size 4 bytes
FIX: Command register 0x0000 — enabling MEM + BUS MASTER → 0x0006
00000000  00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00  |................|
00000010  00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00  |................|
00000020
```

## Notes

- For MMIO reads, `pcipoke` will automatically align accesses to the specified `read-size`.
- Many MMIO registers require naturally-aligned accesses for writes; misaligned offsets may fault or silently misbehave depending on the device. It's best to specify an `offset` that aligns to the write size.
- The hexdump output is color-coded: null bytes are dark grey, printable ASCII is green, `0xff` is red, and other high bytes are yellow.
- Diagnostic output (device status, errors) is written to **stderr**; hexdump data is written to **stdout**, so output can be piped or redirected independently.
