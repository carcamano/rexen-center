//! OVA archives (F11b): a tar file holding an OVF package.
//!
//! * [`index`] lists the entries of an OVA by reading only their headers
//!   (each entry's content is skipped with a seek), so a package of any
//!   size is looked at without reading its disks.
//! * [`TarWriter`] writes one entry at a time. An entry whose size is not
//!   known in advance (a disk streamed from the server) gets a placeholder
//!   header that is written again once its content is in: the archive is
//!   made in one pass, without a copy of the package next to it.
//!
//! Names longer than 100 bytes use the ustar prefix field when they can,
//! else a GNU long-name entry; both are read back.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// One file of an OVA: its name, where its content starts and its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvaEntry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

fn padding(size: u64) -> u64 {
    (512 - size % 512) % 512
}

fn octal(field: &[u8]) -> Option<u64> {
    // GNU base-256 for sizes over 8 GiB.
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        let mut v = u64::from(field[0] & 0x7f);
        for b in &field[1..] {
            v = v.checked_mul(256)? + u64::from(*b);
        }
        return Some(v);
    }
    let s: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Why an OVA could not be read.
#[derive(Debug)]
pub enum OvaError {
    Io(std::io::Error),
    /// A header is not a tar header (a bad checksum or size).
    NotTar,
}

impl std::fmt::Display for OvaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::NotTar => f.write_str("the file is not a valid OVA (tar) archive"),
        }
    }
}

impl std::error::Error for OvaError {}

impl From<std::io::Error> for OvaError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// The regular files of the OVA at `path`, in archive order.
pub fn index(path: &Path) -> Result<Vec<OvaEntry>, OvaError> {
    let mut f = std::fs::File::open(path)?;
    let length = f.metadata()?.len();
    let mut out = Vec::new();
    let mut pos = 0u64;
    let mut long_name: Option<String> = None;
    let mut h = [0u8; 512];
    loop {
        if pos + 512 > length {
            // No end marker: tolerated, as most readers do.
            break;
        }
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut h)?;
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let stored = octal(&h[148..156]).ok_or(OvaError::NotTar)?;
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(*b)
                }
            })
            .sum();
        if sum != stored {
            return Err(OvaError::NotTar);
        }
        let size = octal(&h[124..136]).ok_or(OvaError::NotTar)?;
        let start = pos + 512;
        if start + size > length {
            return Err(OvaError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the archive ends inside an entry",
            )));
        }
        let kind = h[156];
        match kind {
            b'L' => {
                let mut name = vec![0u8; usize::try_from(size).map_err(|_| OvaError::NotTar)?];
                f.read_exact(&mut name)?;
                long_name = Some(text(&name));
            }
            b'0' | 0 | b'7' => {
                let name = long_name.take().unwrap_or_else(|| {
                    let base = text(&h[..100]);
                    let prefix = if &h[257..262] == b"ustar" {
                        text(&h[345..500])
                    } else {
                        String::new()
                    };
                    if prefix.is_empty() {
                        base
                    } else {
                        format!("{prefix}/{base}")
                    }
                });
                out.push(OvaEntry {
                    name: name.trim_start_matches("./").to_owned(),
                    offset: start,
                    size,
                });
            }
            // Directories, pax headers and the like.
            _ => long_name = None,
        }
        pos = start + size + padding(size);
    }
    Ok(out)
}

/// A ustar header for `name` and `size` (`None` when the name does not
/// fit, even split).
fn header(name: &str, size: u64, kind: u8) -> Option<[u8; 512]> {
    let mut h = [0u8; 512];
    let bytes = name.as_bytes();
    if bytes.len() <= 100 {
        h[..bytes.len()].copy_from_slice(bytes);
    } else {
        // prefix (155) + '/' + name (100).
        let split = (0..bytes.len())
            .rev()
            .find(|&i| bytes[i] == b'/' && i <= 155 && bytes.len() - i - 1 <= 100)?;
        h[345..345 + split].copy_from_slice(&bytes[..split]);
        h[..bytes.len() - split - 1].copy_from_slice(&bytes[split + 1..]);
    }
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    if size < 8 << 30 {
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    } else {
        h[124] = 0x80;
        h[128..136].copy_from_slice(&size.to_be_bytes());
    }
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    h[136..148].copy_from_slice(format!("{mtime:011o}\0").as_bytes());
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    Some(h)
}

/// Writes a tar archive into a seekable file, one entry at a time.
pub struct TarWriter<W: Write + Seek> {
    out: W,
    /// The entry being written: where its header is, its name and the
    /// bytes written so far.
    open: Option<(u64, String, u64)>,
}

impl<W: Write + Seek> TarWriter<W> {
    pub fn new(out: W) -> Self {
        Self { out, open: None }
    }

    fn name_headers(&mut self, name: &str, size: u64) -> std::io::Result<()> {
        if let Some(h) = header(name, size, b'0') {
            return self.out.write_all(&h);
        }
        // GNU long name.
        let mut long = name.as_bytes().to_vec();
        long.push(0);
        let h = header("././@LongLink", long.len() as u64, b'L').expect("short");
        self.out.write_all(&h)?;
        self.out.write_all(&long)?;
        self.out
            .write_all(&vec![0u8; padding(long.len() as u64) as usize])?;
        let short: String = name
            .chars()
            .rev()
            .take(99)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        self.out
            .write_all(&header(&short, size, b'0').expect("short"))
    }

    /// A whole entry.
    pub fn entry(&mut self, name: &str, content: &[u8]) -> std::io::Result<()> {
        self.begin(name)?;
        self.write(content)?;
        self.end()
    }

    /// Starts an entry whose size is not known yet.
    pub fn begin(&mut self, name: &str) -> std::io::Result<()> {
        if self.open.is_some() {
            self.end()?;
        }
        let at = self.out.stream_position()?;
        self.name_headers(name, 0)?;
        self.open = Some((at, name.to_owned(), 0));
        Ok(())
    }

    /// Content of the open entry.
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        let Some((_, _, n)) = self.open.as_mut() else {
            return Err(std::io::Error::other("no tar entry is open"));
        };
        self.out.write_all(data)?;
        *n += data.len() as u64;
        Ok(())
    }

    /// Ends the open entry: its padding, then its header with the size.
    pub fn end(&mut self) -> std::io::Result<()> {
        let Some((at, name, size)) = self.open.take() else {
            return Ok(());
        };
        self.out.write_all(&vec![0u8; padding(size) as usize])?;
        let here = self.out.stream_position()?;
        self.out.seek(SeekFrom::Start(at))?;
        self.name_headers(&name, size)?;
        self.out.seek(SeekFrom::Start(here))?;
        Ok(())
    }

    /// The end marker; returns the file.
    pub fn finish(mut self) -> std::io::Result<W> {
        self.end()?;
        self.out.write_all(&[0u8; 1024])?;
        self.out.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_archives_are_indexed() {
        let dir = std::env::temp_dir().join(format!("xen-ova-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.ova");
        let long = format!("{}/disk.vhd", "d".repeat(120));
        let longer = "x".repeat(300);
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut t = TarWriter::new(f);
            t.entry("pkg.ovf", b"<Envelope/>").unwrap();
            t.begin("disk1.vhd").unwrap();
            for _ in 0..3 {
                t.write(&[5u8; 700]).unwrap();
            }
            t.entry(&long, b"abc").unwrap();
            t.entry(&longer, b"z").unwrap();
            t.entry("pkg.mf", b"").unwrap();
            t.finish().unwrap();
        }
        let e = index(&path).unwrap();
        let names: Vec<_> = e.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["pkg.ovf", "disk1.vhd", &long, &longer, "pkg.mf"]);
        assert_eq!(e[1].size, 2100);
        let mut f = std::fs::File::open(&path).unwrap();
        let mut b = vec![0u8; 3];
        f.seek(SeekFrom::Start(e[2].offset)).unwrap();
        f.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"abc");
        // The system's tar reads it too.
        let listed = std::process::Command::new("tar")
            .arg("tf")
            .arg(&path)
            .output();
        if let Ok(out) = listed
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.contains("disk1.vhd") && text.contains(&long), "{text}");
        }
        // A corrupt header is refused.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[3] ^= 0x20;
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(index(&path), Err(OvaError::NotTar)));
    }

    #[test]
    fn base256_sizes() {
        let mut f = [0u8; 12];
        f[0] = 0x80;
        f[4..12].copy_from_slice(&(9u64 << 30).to_be_bytes());
        assert_eq!(octal(&f), Some(9 << 30));
    }
}
