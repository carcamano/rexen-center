//! Disk image files (F11b): what a file holds, read from its first and
//! last bytes, never from the whole of it.
//!
//! XAPI's `/import_raw_vdi` takes a VHD (`format=vhd`, fixed or dynamic) or
//! raw bytes (`format=raw`: a raw image or an ISO). Nothing is converted
//! here (plan decision 12, no external converter): VMDK, VHDX, qcow2 and WIM
//! files are recognised only to be refused with a clear message, and so is
//! a differencing VHD, which needs its parent.
//!
//! [`fixed_vhd_footer`] writes the 512-byte footer of a fixed VHD, for tests
//! and tools that make small images.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The 512-byte VHD footer's cookie.
const VHD_COOKIE: &[u8; 8] = b"conectix";

/// What a disk image file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// A dynamic VHD (uploaded with `format=vhd`).
    Vhd,
    /// A fixed VHD: raw bytes followed by the footer. Its first
    /// `virtual_size` bytes are uploaded with `format=raw`, the footer left
    /// out (XAPI reads a VHD stream from its start, where a fixed VHD has
    /// no header).
    FixedVhd,
    /// Raw bytes (uploaded with `format=raw`).
    Raw,
    /// An ISO 9660 image (raw bytes, `format=raw`).
    Iso,
}

impl ImageFormat {
    /// The bytes of a file of `file_size` holding `virtual_size` bytes of
    /// disk that are uploaded.
    pub fn upload_length(self, virtual_size: u64, file_size: u64) -> u64 {
        match self {
            Self::FixedVhd => virtual_size,
            _ => file_size,
        }
    }

    /// The `format` parameter of `/import_raw_vdi` and `/export_raw_vdi`.
    pub fn transfer_format(self) -> &'static str {
        match self {
            Self::Vhd => "vhd",
            Self::FixedVhd | Self::Raw | Self::Iso => "raw",
        }
    }
}

/// A format this console does not convert (refused by the wizards).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    Vmdk,
    Vhdx,
    Qcow2,
    Wim,
    /// A differencing VHD: its content needs its parent.
    DifferencingVhd,
}

impl Unsupported {
    /// The format's usual name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vmdk => "VMDK",
            Self::Vhdx => "VHDX",
            Self::Qcow2 => "qcow2",
            Self::Wim => "WIM",
            Self::DifferencingVhd => "differencing VHD",
        }
    }
}

/// What [`inspect`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    /// An image that can be uploaded: its format, the size of the disk it
    /// holds (the VHD's current size, else the file's size) and the
    /// file's size.
    Supported {
        format: ImageFormat,
        virtual_size: u64,
        file_size: u64,
    },
    /// A format that would need converting.
    Unsupported(Unsupported),
}

/// A VHD footer's fields this console uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VhdFooter {
    /// The size of the disk, in bytes.
    pub current_size: u64,
    /// 2 fixed, 3 dynamic, 4 differencing.
    pub disk_type: u32,
}

/// Reads a VHD footer (or the copy at the start of a dynamic VHD). `None`
/// when `b` is not one: no cookie, or a wrong checksum.
pub fn parse_vhd_footer(b: &[u8]) -> Option<VhdFooter> {
    if b.len() < 512 || &b[..8] != VHD_COOKIE {
        return None;
    }
    let be64 = |o: usize| u64::from_be_bytes(b[o..o + 8].try_into().expect("8 bytes"));
    let be32 = |o: usize| u32::from_be_bytes(b[o..o + 4].try_into().expect("4 bytes"));
    let sum: u32 = b[..512]
        .iter()
        .enumerate()
        .filter(|(i, _)| !(64..68).contains(i))
        .map(|(_, x)| u32::from(*x))
        .sum();
    if !sum != be32(64) {
        return None;
    }
    Some(VhdFooter {
        current_size: be64(48),
        disk_type: be32(60),
    })
}

/// The footer of a fixed VHD of `size` bytes (appended to the raw bytes).
pub fn fixed_vhd_footer(size: u64, uuid: [u8; 16]) -> [u8; 512] {
    let mut f = [0u8; 512];
    f[..8].copy_from_slice(VHD_COOKIE);
    f[8..12].copy_from_slice(&2u32.to_be_bytes()); // features: reserved
    f[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // version 1.0
    f[16..24].copy_from_slice(&u64::MAX.to_be_bytes()); // no dynamic header
    f[28..32].copy_from_slice(b"xrs ");
    f[32..36].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    f[36..40].copy_from_slice(b"Wi2k");
    f[40..48].copy_from_slice(&size.to_be_bytes());
    f[48..56].copy_from_slice(&size.to_be_bytes());
    f[56..60].copy_from_slice(&chs(size).to_be_bytes());
    f[60..64].copy_from_slice(&2u32.to_be_bytes());
    f[68..84].copy_from_slice(&uuid);
    let sum: u32 = f.iter().map(|x| u32::from(*x)).sum();
    f[64..68].copy_from_slice(&(!sum).to_be_bytes());
    f
}

/// The VHD specification's CHS geometry of `size` bytes.
fn chs(size: u64) -> u32 {
    let mut sectors = (size / 512).min(65535 * 16 * 255);
    let (spt, heads, cyl_heads);
    if sectors >= 65535 * 16 * 63 {
        spt = 255;
        heads = 16;
        cyl_heads = sectors / spt;
    } else {
        let mut s = 17;
        let mut ch = sectors / s;
        let mut h = ch.div_ceil(1024).max(4);
        if ch >= h * 1024 || h > 16 {
            s = 31;
            h = 16;
            ch = sectors / s;
        }
        if ch >= h * 1024 {
            s = 63;
            h = 16;
            ch = sectors / s;
        }
        spt = s;
        heads = h;
        cyl_heads = ch;
    }
    let cylinders = cyl_heads / heads;
    sectors = cylinders;
    ((sectors as u32) << 16) | ((heads as u32) << 8) | spt as u32
}

/// Looks at the file at `path`: its first sectors, the ISO volume
/// descriptor and its last 512 bytes.
pub fn inspect(path: &Path) -> std::io::Result<Image> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    inspect_at(&mut f, 0, len)
}

/// [`inspect`] of the `len` bytes at `offset` of `f` (a file inside an
/// OVA).
pub fn inspect_at<F: Read + Seek>(f: &mut F, offset: u64, len: u64) -> std::io::Result<Image> {
    let file_size = len;
    let mut head = [0u8; 512];
    f.seek(SeekFrom::Start(offset))?;
    let got = read_up_to(&mut f.take(len.min(512)), &mut head)?;
    let head = &head[..got];
    let starts = |magic: &[u8]| head.len() >= magic.len() && &head[..magic.len()] == magic;
    if starts(b"KDMV") || starts(b"COWD") || starts(b"# Disk DescriptorFile") {
        return Ok(Image::Unsupported(Unsupported::Vmdk));
    }
    if starts(b"vhdxfile") {
        return Ok(Image::Unsupported(Unsupported::Vhdx));
    }
    if starts(b"QFI\xfb") {
        return Ok(Image::Unsupported(Unsupported::Qcow2));
    }
    if starts(b"MSWIM\0\0\0") {
        return Ok(Image::Unsupported(Unsupported::Wim));
    }
    let vhd = |footer: VhdFooter, dynamic: bool| {
        if footer.disk_type == 4 {
            Image::Unsupported(Unsupported::DifferencingVhd)
        } else {
            Image::Supported {
                format: if dynamic {
                    ImageFormat::Vhd
                } else {
                    ImageFormat::FixedVhd
                },
                virtual_size: footer.current_size,
                file_size,
            }
        }
    };
    // A dynamic VHD starts with a copy of its footer.
    if let Some(footer) = parse_vhd_footer(head) {
        return Ok(vhd(footer, footer.disk_type != 2));
    }
    if file_size >= 512 {
        let mut tail = [0u8; 512];
        f.seek(SeekFrom::Start(offset + file_size - 512))?;
        f.read_exact(&mut tail)?;
        if let Some(footer) = parse_vhd_footer(&tail) {
            // A fixed VHD: raw bytes, then the footer.
            if footer.disk_type == 2 && footer.current_size > file_size - 512 {
                return Ok(Image::Supported {
                    format: ImageFormat::Raw,
                    virtual_size: file_size,
                    file_size,
                });
            }
            return Ok(vhd(footer, footer.disk_type != 2));
        }
    }
    // ISO 9660: "CD001" in the first volume descriptor (sector 16).
    if file_size >= 0x8006 {
        let mut id = [0u8; 6];
        f.seek(SeekFrom::Start(offset + 0x8000))?;
        f.read_exact(&mut id)?;
        if &id[1..6] == b"CD001" {
            return Ok(Image::Supported {
                format: ImageFormat::Iso,
                virtual_size: file_size,
                file_size,
            });
        }
    }
    Ok(Image::Supported {
        format: ImageFormat::Raw,
        virtual_size: file_size,
        file_size,
    })
}

/// Checks the structure of a VHD read from `r` (`len` bytes), as the
/// original's verification of an exported disk: the footer copy and the
/// final footer (cookie, checksum, the same), the dynamic header's
/// checksum, and every block of the block table inside the file.
pub fn check_vhd(r: &mut impl Read, len: u64) -> Result<VhdFooter, String> {
    if len < 512 {
        return Err("the file is too small to be a VHD".into());
    }
    let mut at = 0u64;
    let skip_to = |r: &mut dyn Read, at: &mut u64, to: u64| -> Result<(), String> {
        if to < *at {
            return Err("the VHD's parts overlap".into());
        }
        let n = std::io::copy(&mut r.take(to - *at), &mut std::io::sink())
            .map_err(|e| e.to_string())?;
        *at += n;
        if *at != to {
            return Err("the VHD ends early".into());
        }
        Ok(())
    };
    let read = |r: &mut dyn Read, at: &mut u64, buf: &mut [u8]| -> Result<(), String> {
        r.read_exact(buf)
            .map_err(|_| "the VHD ends early".to_string())?;
        *at += buf.len() as u64;
        Ok(())
    };
    let mut first = [0u8; 512];
    read(r, &mut at, &mut first)?;
    let Some(copy) = parse_vhd_footer(&first) else {
        // A fixed VHD: only its final footer.
        skip_to(r, &mut at, len - 512)?;
        let mut last = [0u8; 512];
        read(r, &mut at, &mut last)?;
        return parse_vhd_footer(&last).ok_or_else(|| "the VHD footer is invalid".to_string());
    };
    if copy.disk_type != 3 {
        return Err("not a dynamic VHD".into());
    }
    let be64 = |b: &[u8], o: usize| u64::from_be_bytes(b[o..o + 8].try_into().expect("8"));
    let be32 = |b: &[u8], o: usize| u32::from_be_bytes(b[o..o + 4].try_into().expect("4"));
    let header_at = be64(&first, 16);
    skip_to(r, &mut at, header_at)?;
    let mut header = [0u8; 1024];
    read(r, &mut at, &mut header)?;
    let sum: u32 = header
        .iter()
        .enumerate()
        .filter(|(i, _)| !(36..40).contains(i))
        .map(|(_, x)| u32::from(*x))
        .sum();
    if &header[..8] != b"cxsparse" || !sum != be32(&header, 36) {
        return Err("the VHD header is invalid".into());
    }
    let table = be64(&header, 16);
    let entries = u64::from(be32(&header, 28));
    let block = u64::from(be32(&header, 32));
    if block == 0 || entries.saturating_mul(block) < copy.current_size {
        return Err("the VHD block table does not cover the disk".into());
    }
    skip_to(r, &mut at, table)?;
    let mut bat = vec![0u8; usize::try_from(entries * 4).map_err(|e| e.to_string())?];
    read(r, &mut at, &mut bat)?;
    let bitmap = (block / 512 / 8).div_ceil(512) * 512;
    for i in 0..entries as usize {
        let s = be32(&bat, i * 4);
        if s != u32::MAX && u64::from(s) * 512 + bitmap + block > len - 512 {
            return Err(format!("block {i} of the VHD lies outside the file"));
        }
    }
    skip_to(r, &mut at, len - 512)?;
    let mut last = [0u8; 512];
    read(r, &mut at, &mut last)?;
    match parse_vhd_footer(&last) {
        Some(f) if f == copy && last == first => Ok(f),
        _ => Err("the VHD's final footer is invalid".into()),
    }
}

/// The SHA-256 of the disk a VHD holds (its `current_size` bytes, the
/// blocks a dynamic VHD leaves out read as zeros), from the `len` bytes at
/// `offset` of `f`; with the disk's size. A test of what a transfer
/// carried.
pub fn vhd_content_sha256<F: Read + Seek>(
    f: &mut F,
    offset: u64,
    len: u64,
) -> std::io::Result<(Vec<u8>, u64)> {
    let bad = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_owned());
    let mut first = [0u8; 512];
    f.seek(SeekFrom::Start(offset))?;
    f.read_exact(&mut first)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let Some(footer) = parse_vhd_footer(&first).filter(|f| f.disk_type == 3) else {
        // Fixed: the bytes before the footer.
        let mut last = [0u8; 512];
        f.seek(SeekFrom::Start(offset + len - 512))?;
        f.read_exact(&mut last)?;
        let footer = parse_vhd_footer(&last).ok_or_else(|| bad("not a VHD"))?;
        f.seek(SeekFrom::Start(offset))?;
        let mut r = f.take(footer.current_size);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            ctx.update(&buf[..n]);
        }
        return Ok((ctx.finish().as_ref().to_vec(), footer.current_size));
    };
    let be64 = |b: &[u8], o: usize| u64::from_be_bytes(b[o..o + 8].try_into().expect("8"));
    let be32 = |b: &[u8], o: usize| u32::from_be_bytes(b[o..o + 4].try_into().expect("4"));
    let mut header = [0u8; 1024];
    f.seek(SeekFrom::Start(offset + be64(&first, 16)))?;
    f.read_exact(&mut header)?;
    let table = be64(&header, 16);
    let entries = u64::from(be32(&header, 28));
    let block = u64::from(be32(&header, 32));
    let bitmap = (block / 512 / 8).div_ceil(512) * 512;
    let mut bat = vec![0u8; (entries * 4) as usize];
    f.seek(SeekFrom::Start(offset + table))?;
    f.read_exact(&mut bat)?;
    let zeros = vec![0u8; block as usize];
    let mut data = vec![0u8; block as usize];
    let mut left = footer.current_size;
    for i in 0..entries as usize {
        if left == 0 {
            break;
        }
        let n = left.min(block) as usize;
        let s = be32(&bat, i * 4);
        if s == u32::MAX {
            ctx.update(&zeros[..n]);
        } else {
            f.seek(SeekFrom::Start(offset + u64::from(s) * 512 + bitmap))?;
            f.read_exact(&mut data[..n])?;
            ctx.update(&data[..n]);
        }
        left -= n as u64;
    }
    if left > 0 {
        return Err(bad("the block table does not cover the disk"));
    }
    Ok((ctx.finish().as_ref().to_vec(), footer.current_size))
}

/// The SHA-256 of everything `r` holds.
pub fn sha256_of(r: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().to_vec())
}

fn read_up_to(f: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// `size` rounded up to whole megabytes (VDI sizes the original creates
/// from an image).
pub fn round_up_mib(size: u64) -> u64 {
    size.div_ceil(1 << 20) * (1 << 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("xen-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn fixed_vhds_raw_and_iso_are_recognised() {
        let size = 3u64 << 20;
        let mut vhd = vec![7u8; size as usize];
        vhd.extend_from_slice(&fixed_vhd_footer(size, [1; 16]));
        assert_eq!(
            inspect(&temp("a.vhd", &vhd)).unwrap(),
            Image::Supported {
                format: ImageFormat::FixedVhd,
                virtual_size: size,
                file_size: size + 512
            }
        );
        let raw = vec![0u8; 4096];
        assert_eq!(
            inspect(&temp("a.img", &raw)).unwrap(),
            Image::Supported {
                format: ImageFormat::Raw,
                virtual_size: 4096,
                file_size: 4096
            }
        );
        let mut iso = vec![0u8; 0x9000];
        iso[0x8000] = 1;
        iso[0x8001..0x8006].copy_from_slice(b"CD001");
        assert!(matches!(
            inspect(&temp("a.iso", &iso)).unwrap(),
            Image::Supported {
                format: ImageFormat::Iso,
                ..
            }
        ));
        // A broken footer is not a VHD.
        let mut bad = vhd.clone();
        let n = bad.len();
        bad[n - 400] ^= 1;
        assert!(matches!(
            inspect(&temp("b.vhd", &bad)).unwrap(),
            Image::Supported {
                format: ImageFormat::Raw,
                ..
            }
        ));
    }

    #[test]
    fn formats_that_need_converting_are_refused() {
        for (magic, kind) in [
            (&b"KDMV\x01\0\0\0"[..], Unsupported::Vmdk),
            (b"# Disk DescriptorFile\n", Unsupported::Vmdk),
            (b"vhdxfile", Unsupported::Vhdx),
            (b"QFI\xfb\0\0\0\x03", Unsupported::Qcow2),
            (b"MSWIM\0\0\0", Unsupported::Wim),
        ] {
            let mut b = magic.to_vec();
            b.resize(8192, 0);
            assert_eq!(inspect(&temp("x", &b)).unwrap(), Image::Unsupported(kind));
        }
        let mut diff = fixed_vhd_footer(1 << 20, [2; 16]);
        diff[60..64].copy_from_slice(&4u32.to_be_bytes());
        diff[64..68].copy_from_slice(&[0; 4]);
        let sum: u32 = diff.iter().map(|x| u32::from(*x)).sum();
        diff[64..68].copy_from_slice(&(!sum).to_be_bytes());
        assert_eq!(
            inspect(&temp("d.vhd", &diff)).unwrap(),
            Image::Unsupported(Unsupported::DifferencingVhd)
        );
    }

    #[test]
    fn geometry() {
        // 100 MiB: the geometry Hyper-V writes.
        let g = chs(100 << 20);
        assert_eq!(g & 0xff, 17);
        assert_eq!((g >> 8) & 0xff, 12);
    }
}
