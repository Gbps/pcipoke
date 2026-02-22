use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Parser;
use colored::Colorize;
use memmap2::MmapOptions;

/// Read/write PCI device memory from userspace via sysfs BAR resources
#[derive(Parser)]
#[command(name = "pcipoke", version, about)]
struct Cli {
    /// PCI address in [DDDD:]BB:DD.F form (e.g. 0000:01:00.0)
    address: String,

    /// Operation: 'r' to read, 'w' to write
    operation: char,

    /// For 'r': number of bytes to read (supports 0x hex prefix).
    /// For 'w': data value to write expressed in hex (e.g. 0xDEADBEEF or DEADBEEF).
    #[arg(value_name = "COUNT|DATA")]
    operand: String,

    /// BAR number (0-5). 64-bit BARs consume the next BAR number as well. Default is 0.
    #[arg(short = 'b', long, default_value = "0", value_parser = parse_bar)]
    bar_num: usize,

    /// Offset into the BAR to start from (supports 0x hex prefix)
    #[arg(short, long, default_value = "0", value_parser = parse_count)]
    offset: usize,

    /// Size of each individual MMIO read in bytes (1, 2, 4, or 8)
    #[arg(short = 's', long, default_value = "4", value_parser = parse_read_size)]
    read_size: usize,

    /// Size of the MMIO write in bytes (1, 2, 4, or 8)
    #[arg(short = 'w', long, default_value = "4", value_parser = parse_write_size)]
    write_size: usize,
}

fn parse_bar(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e| format!("invalid number: {e}"))?;
    if n <= 5 {
        Ok(n)
    } else {
        Err("BAR number must be between 0 and 5".to_string())
    }
}

fn parse_read_size(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e| format!("invalid number: {e}"))?;
    match n {
        1 | 2 | 4 | 8 => Ok(n),
        _ => Err("read size must be 1, 2, 4, or 8".to_string()),
    }
}

fn parse_write_size(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e| format!("invalid number: {e}"))?;
    match n {
        1 | 2 | 4 | 8 => Ok(n),
        _ => Err("write size must be 1, 2, 4, or 8".to_string()),
    }
}

fn parse_count(s: &str) -> Result<usize, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        usize::from_str_radix(hex, 16).map_err(|e| format!("invalid hex number: {e}"))
    } else {
        s.parse::<usize>().map_err(|e| format!("invalid number: {e}"))
    }
}

fn parse_hex_u64(s: &str) -> Result<u64, String> {
    let hex = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex data: {e}"))
}

/// Validate and normalize a PCI address to DDDD:BB:DD.F form.
fn normalize_pci_address(addr: &str) -> Result<String> {
    let full = match addr.matches(':').count() {
        1 => format!("0000:{addr}"),
        2 => addr.to_string(),
        _ => bail!("Invalid PCI address format. Expected [DDDD:]BB:DD.F (e.g. 0000:01:00.0)"),
    };

    let parts: Vec<&str> = full.split(':').collect();
    if parts.len() != 3 {
        bail!("Invalid PCI address format");
    }

    // Domain: 4 hex digits
    if parts[0].len() != 4 || !parts[0].chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid domain '{}' in PCI address (expected 4 hex digits)", parts[0]);
    }

    // Bus: 2 hex digits
    if parts[1].len() != 2 || !parts[1].chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid bus '{}' in PCI address (expected 2 hex digits)", parts[1]);
    }

    // Device.Function: DD.F
    let dev_fn: Vec<&str> = parts[2].split('.').collect();
    if dev_fn.len() != 2 {
        bail!("Invalid device.function '{}' (expected DD.F)", parts[2]);
    }
    if dev_fn[0].len() != 2 || !dev_fn[0].chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid device '{}' (expected 2 hex digits)", dev_fn[0]);
    }
    if dev_fn[1].len() != 1 || !dev_fn[1].chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid function '{}' (expected 1 hex digit)", dev_fn[1]);
    }

    Ok(full)
}

/// Read the PCI command register via setpci. Enable MEM and BUS MASTER if not already set.
fn check_and_enable_device(addr: &str) -> Result<()> {
    let output = Command::new("setpci")
        .args(["-s", addr, "COMMAND"])
        .output()
        .context("Failed to run setpci — is pciutils installed?")?;

    if !output.status.success() {
        bail!(
            "setpci read failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let cmd_str = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // If stdout has no output, the device doesn't exist or isn't accessible.
    if cmd_str.is_empty() {
        bail!("Device {addr} not found or not accessible.");
    }

    let cmd_val =
        u16::from_str_radix(&cmd_str, 16).context("Failed to parse command register value")?;

    let mem_enabled = cmd_val & 0x0002 != 0;
    let bm_enabled = cmd_val & 0x0004 != 0;

    if mem_enabled && bm_enabled {
        eprintln!(
            "{} Command register {:#06x} — MEM and BUS MASTER already enabled",
            "OK:".green().bold(),
            cmd_val
        );
        return Ok(());
    }

    let new_val = cmd_val | 0x0006;
    eprintln!(
        "{} Command register {:#06x} — enabling MEM + BUS MASTER → {:#06x}",
        "FIX:".yellow().bold(),
        cmd_val,
        new_val
    );

    let output = Command::new("setpci")
        .args(["-s", addr, &format!("COMMAND={new_val:04x}")])
        .output()
        .context("Failed to run setpci to enable device")?;

    if !output.status.success() {
        bail!(
            "setpci write failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(())
}

/// mmap resource and read the requested range using volatile accesses of `read_size` width.
/// If `count` is not a multiple of `read_size`, the last read is still full-width and the
/// result is truncated to exactly `count` bytes.
fn read_resource(addr: &str, bar_num: usize, offset: usize, count: usize, read_size: usize) -> Result<Vec<u8>> {
    let path = PathBuf::from(format!("/sys/bus/pci/devices/{addr}/resource{bar_num}"));

    if !path.exists() {
        bail!("Resource file not found: {}", path.display());
    }

    let file = OpenOptions::new()
        .read(true)
        .write(false)
        .open(&path)
        .with_context(|| format!("Failed to open {} — are you running as root?", path.display()))?;

    // Round up so the mmap covers every read we will issue.
    let aligned_count = (count + read_size - 1) / read_size * read_size;

    let mmap = unsafe {
        MmapOptions::new()
            .offset(offset as u64)
            .len(aligned_count)
            .map(&file)
            .with_context(|| format!("Failed to mmap {} ({aligned_count} bytes)", path.display()))?
    };

    let base = mmap.as_ptr();
    let mut buf = Vec::with_capacity(aligned_count);

    for i in (0..aligned_count).step_by(read_size) {
        unsafe {
            match read_size {
                1 => {
                    let v = std::ptr::read_volatile(base.add(i));
                    buf.push(v);
                }
                2 => {
                    let v = std::ptr::read_volatile(base.add(i) as *const u16);
                    buf.extend_from_slice(&v.to_ne_bytes());
                }
                4 => {
                    let v = std::ptr::read_volatile(base.add(i) as *const u32);
                    buf.extend_from_slice(&v.to_ne_bytes());
                }
                8 => {
                    let v = std::ptr::read_volatile(base.add(i) as *const u64);
                    buf.extend_from_slice(&v.to_ne_bytes());
                }
                _ => unreachable!(),
            }
        }
    }

    buf.truncate(count);
    Ok(buf)
}

/// mmap resource with write access and issue a single volatile write of `write_size` width.
fn write_resource(addr: &str, bar_num: usize, offset: usize, data: u64, write_size: usize) -> Result<()> {
    let path = PathBuf::from(format!("/sys/bus/pci/devices/{addr}/resource{bar_num}"));

    if !path.exists() {
        bail!("Resource file not found: {}", path.display());
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("Failed to open {} — are you running as root?", path.display()))?;

    let mut mmap = unsafe {
        MmapOptions::new()
            .offset(offset as u64)
            .len(write_size)
            .map_mut(&file)
            .with_context(|| format!("Failed to mmap {} ({write_size} bytes)", path.display()))?
    };

    let base = mmap.as_mut_ptr();

    unsafe {
        match write_size {
            1 => std::ptr::write_volatile(base, data as u8),
            2 => std::ptr::write_volatile(base as *mut u16, data as u16),
            4 => std::ptr::write_volatile(base as *mut u32, data as u32),
            8 => std::ptr::write_volatile(base as *mut u64, data as u64),
            _ => unreachable!(),
        }
    }

    Ok(())
}

// -- Hexdump rendering -----------------------------------------------

fn color_byte_hex(b: u8) -> colored::ColoredString {
    let s = format!("{b:02x}");
    match b {
        0x00 => s.bright_black(),
        0x20..=0x7e => s.green(),
        0xff => s.red(),
        0x80.. => s.yellow(),
        _ => s.cyan(),
    }
}

fn color_byte_ascii(b: u8) -> colored::ColoredString {
    match b {
        0x00 => ".".bright_black(),
        0x20..=0x7e => format!("{}", b as char).green(),
        0xff => ".".red(),
        0x80.. => ".".yellow(),
        _ => ".".cyan(),
    }
}

fn hexdump(data: &[u8], base_offset: usize) {
    for (i, chunk) in data.chunks(16).enumerate() {
        let addr = base_offset + i * 16;

        // Offset column
        print!("{}", format!("{addr:08x}  ").blue().bold());

        // Hex bytes — two groups of 8
        for (j, &b) in chunk.iter().enumerate() {
            print!("{} ", color_byte_hex(b));
            if j == 7 {
                print!(" ");
            }
        }

        // Pad short final line
        for j in chunk.len()..16 {
            print!("   ");
            if j == 7 {
                print!(" ");
            }
        }

        // ASCII sidebar
        print!(" {}", "|".bright_black());
        for &b in chunk {
            print!("{}", color_byte_ascii(b));
        }
        for _ in chunk.len()..16 {
            print!(" ");
        }
        println!("{}", "|".bright_black());
    }

    // Trailing size line
    let end = base_offset + data.len();
    println!("{}", format!("{end:08x}").blue().bold());
}

// -- Main ------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    let addr = normalize_pci_address(&cli.address)?;

    match cli.operation {
        'r' => {
            let count = parse_count(&cli.operand)
                .map_err(|e| anyhow::anyhow!("invalid COUNT: {e}"))?;

            if count == 0 {
                bail!("Byte count must be greater than 0");
            }

            eprintln!(
                "{} {} resource{}  offset {:#x}  length {:#x} ({} bytes)  read-size {} bytes",
                "READ:".blue().bold(),
                addr,
                cli.bar_num,
                cli.offset,
                count,
                count,
                cli.read_size
            );

            check_and_enable_device(&addr)?;

            let data = read_resource(&addr, cli.bar_num, cli.offset, count, cli.read_size)?;

            hexdump(&data, cli.offset);
        }

        'w' => {
            let data = parse_hex_u64(&cli.operand)
                .map_err(|e| anyhow::anyhow!("invalid DATA: {e}"))?;

            // Validate data fits in the requested write width.
            if cli.write_size < 8 {
                let max = (1u64 << (cli.write_size * 8)) - 1;
                if data > max {
                    bail!(
                        "Data value {:#x} does not fit in {} byte(s) (max {:#x})",
                        data,
                        cli.write_size,
                        max
                    );
                }
            }

            eprintln!(
                "{} {} resource{}  offset {:#x}  data {:#x}  write-size {} bytes",
                "WRITE:".blue().bold(),
                addr,
                cli.bar_num,
                cli.offset,
                data,
                cli.write_size
            );

            check_and_enable_device(&addr)?;

            write_resource(&addr, cli.bar_num, cli.offset, data, cli.write_size)?;

            eprintln!("{} wrote {:#x} to offset {:#x}", "OK:".green().bold(), data, cli.offset);
        }

        other => {
            bail!("Unknown operation '{other}'. Supported operations: 'r' (read), 'w' (write).");
        }
    }

    Ok(())
}
