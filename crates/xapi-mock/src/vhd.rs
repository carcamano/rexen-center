//! Disk content for `/export_raw_vdi` and `/import_raw_vdi`: sparse
//! content, its digest, dynamic VHDs written from it, and a streaming reader
//! of dynamic VHDs and raw bytes.
//!
//! Content is kept as `CHUNK`-byte chunks by offset; chunks of zeros are
//! not kept. The digest of a disk (`digest`) depends only on its bytes,
//! so a raw image, the VHD made of it and the disk it was imported into all
//! have the same digest.

use std::collections::BTreeMap;

/// The piece of content kept and hashed.
pub(crate) const CHUNK: usize = 64 << 10;
/// The data block of the dynamic VHDs written (XAPI's: 2 MiB).
pub(crate) const VHD_BLOCK: u64 = 2 << 20;

/// Offset → chunk (never all zeros).
pub(crate) type Content = BTreeMap<u64, Vec<u8>>;

fn xxh(data: &[u8]) -> String {
    crate::xva::xxh64_hex(data)
}

/// The digest of `content`: the hash of each non-zero chunk's offset and
/// hash.
pub(crate) fn digest(content: &Content) -> String {
    let mut all = String::new();
    for (offset, chunk) in content {
        if chunk.iter().any(|b| *b != 0) {
            all.push_str(&format!("{offset:x}:{};", xxh(chunk)));
        }
    }
    xxh(all.as_bytes())
}

/// The digest of a disk holding `bytes` from its start (what a test
/// expects of a raw image it uploads).
pub fn content_digest(bytes: &[u8]) -> String {
    let mut c = Content::new();
    for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
        put(&mut c, (i * CHUNK) as u64, chunk);
    }
    digest(&c)
}

/// Stores `data` at `offset` (a chunk boundary), unless all zeros.
fn put(c: &mut Content, offset: u64, data: &[u8]) {
    if data.iter().any(|b| *b != 0) {
        let mut v = data.to_vec();
        v.resize(CHUNK, 0);
        c.insert(offset, v);
    } else {
        c.remove(&offset);
    }
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().expect("4"))
}

fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().expect("8"))
}

fn checksum(b: &[u8], at: usize) -> u32 {
    !b.iter()
        .enumerate()
        .filter(|(i, _)| !(at..at + 4).contains(i))
        .map(|(_, x)| u32::from(*x))
        .sum::<u32>()
}

fn footer(size: u64, disk_type: u32, data_offset: u64) -> Vec<u8> {
    let mut f = vec![0u8; 512];
    f[..8].copy_from_slice(b"conectix");
    f[8..12].copy_from_slice(&2u32.to_be_bytes());
    f[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    f[16..24].copy_from_slice(&data_offset.to_be_bytes());
    f[28..32].copy_from_slice(b"tap\0");
    f[36..40].copy_from_slice(b"Wi2k");
    f[40..48].copy_from_slice(&size.to_be_bytes());
    f[48..56].copy_from_slice(&size.to_be_bytes());
    f[60..64].copy_from_slice(&disk_type.to_be_bytes());
    f[68..84].copy_from_slice(b"xapi-mock-vhd-01");
    let sum = checksum(&f, 64);
    f[64..68].copy_from_slice(&sum.to_be_bytes());
    f
}

/// A dynamic VHD of a `size`-byte disk holding `content`, in pieces.
pub(crate) fn write_dynamic(size: u64, content: &Content) -> Vec<Vec<u8>> {
    let entries = size.div_ceil(VHD_BLOCK);
    let bat_bytes = (entries * 4).div_ceil(512) * 512;
    let table_offset = 512 + 1024;
    let mut header = vec![0u8; 1024];
    header[..8].copy_from_slice(b"cxsparse");
    header[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
    header[16..24].copy_from_slice(&(table_offset as u64).to_be_bytes());
    header[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    header[28..32].copy_from_slice(&(entries as u32).to_be_bytes());
    header[32..36].copy_from_slice(&(VHD_BLOCK as u32).to_be_bytes());
    let sum = checksum(&header, 36);
    header[36..40].copy_from_slice(&sum.to_be_bytes());
    // The blocks with content, in order.
    let mut used: Vec<u64> = content.keys().map(|o| o / VHD_BLOCK).collect();
    used.dedup();
    let bitmap = 512u64;
    let mut bat = vec![0xFFu8; bat_bytes as usize];
    let mut next = (table_offset as u64 + bat_bytes) / 512;
    for b in &used {
        let at = (*b * 4) as usize;
        bat[at..at + 4].copy_from_slice(&(next as u32).to_be_bytes());
        next += (bitmap + VHD_BLOCK) / 512;
    }
    let foot = footer(size, 3, 512);
    let mut out = vec![foot.clone(), header, bat];
    for b in used {
        let mut data = vec![0xFFu8; bitmap as usize];
        let start = b * VHD_BLOCK;
        let mut block = vec![0u8; VHD_BLOCK as usize];
        for (o, chunk) in content.range(start..start + VHD_BLOCK) {
            let at = (o - start) as usize;
            block[at..at + chunk.len()].copy_from_slice(chunk);
        }
        data.extend_from_slice(&block);
        out.push(data);
    }
    out.push(foot);
    out
}

/// Why an upload's content was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) String);

/// Reads an upload as it arrives: raw bytes, or a dynamic VHD.
pub(crate) struct Reader {
    vhd: bool,
    /// The disk's size: content past it is refused.
    capacity: u64,
    buf: Vec<u8>,
    /// Absolute offset of `buf[0]` in the upload.
    at: u64,
    pub(crate) content: Content,
    // VHD state.
    stage: Stage,
    /// File offset → block number, the blocks still to come.
    blocks: BTreeMap<u64, u64>,
    pub(crate) vhd_size: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum Stage {
    Footer,
    Header(u64),
    Bat { offset: u64, entries: u64 },
    Blocks,
    Done,
}

impl Reader {
    pub(crate) fn new(vhd: bool, capacity: u64) -> Self {
        Self {
            vhd,
            capacity,
            buf: Vec::new(),
            at: 0,
            content: Content::new(),
            stage: Stage::Footer,
            blocks: BTreeMap::new(),
            vhd_size: 0,
        }
    }

    /// Takes the next piece of the upload.
    pub(crate) fn feed(&mut self, data: &[u8]) -> Result<(), Refused> {
        self.buf.extend_from_slice(data);
        if self.vhd {
            self.feed_vhd()
        } else {
            self.feed_raw(false)
        }
    }

    /// The upload ended.
    pub(crate) fn finish(&mut self) -> Result<(), Refused> {
        if self.vhd {
            match self.stage {
                // The trailing footer is not needed.
                Stage::Done | Stage::Blocks if self.blocks.is_empty() => Ok(()),
                _ => Err(Refused("the VHD ends early".into())),
            }
        } else {
            self.feed_raw(true)
        }
    }

    fn feed_raw(&mut self, last: bool) -> Result<(), Refused> {
        while self.buf.len() >= CHUNK || (last && !self.buf.is_empty()) {
            let n = self.buf.len().min(CHUNK);
            if self.at + n as u64 > self.capacity {
                return Err(Refused("the content is larger than the disk".into()));
            }
            let chunk: Vec<u8> = self.buf.drain(..n).collect();
            put(&mut self.content, self.at, &chunk);
            self.at += n as u64;
        }
        Ok(())
    }

    /// `len` bytes at absolute `offset` once buffered (earlier bytes
    /// dropped); `None` while they have not all arrived.
    fn take(&mut self, offset: u64, len: usize) -> Result<Option<Vec<u8>>, Refused> {
        if offset < self.at {
            return Err(Refused("the VHD's parts overlap".into()));
        }
        let skip = (offset - self.at) as usize;
        if self.buf.len() < skip {
            self.at += self.buf.len() as u64;
            self.buf.clear();
            return Ok(None);
        }
        self.buf.drain(..skip);
        self.at = offset;
        if self.buf.len() < len {
            return Ok(None);
        }
        let out: Vec<u8> = self.buf.drain(..len).collect();
        self.at += len as u64;
        Ok(Some(out))
    }

    fn feed_vhd(&mut self) -> Result<(), Refused> {
        loop {
            match self.stage {
                Stage::Footer => {
                    let Some(f) = self.take(0, 512)? else {
                        return Ok(());
                    };
                    if &f[..8] != b"conectix" || be32(&f, 64) != checksum(&f, 64) {
                        return Err(Refused("not a dynamic VHD (no footer copy)".into()));
                    }
                    if be32(&f, 60) != 3 {
                        return Err(Refused("not a dynamic VHD".into()));
                    }
                    self.vhd_size = be64(&f, 48);
                    if self.vhd_size > self.capacity {
                        return Err(Refused("the VHD is larger than the disk".into()));
                    }
                    self.stage = Stage::Header(be64(&f, 16));
                }
                Stage::Header(at) => {
                    let Some(h) = self.take(at, 1024)? else {
                        return Ok(());
                    };
                    if &h[..8] != b"cxsparse" || be32(&h, 36) != checksum(&h, 36) {
                        return Err(Refused("a bad VHD header".into()));
                    }
                    if u64::from(be32(&h, 32)) != VHD_BLOCK {
                        return Err(Refused("an unexpected VHD block size".into()));
                    }
                    self.stage = Stage::Bat {
                        offset: be64(&h, 16),
                        entries: u64::from(be32(&h, 28)),
                    };
                }
                Stage::Bat { offset, entries } => {
                    let Some(bat) = self.take(offset, (entries * 4) as usize)? else {
                        return Ok(());
                    };
                    for i in 0..entries {
                        let s = be32(&bat, (i * 4) as usize);
                        if s != u32::MAX {
                            self.blocks.insert(u64::from(s) * 512, i);
                        }
                    }
                    self.stage = Stage::Blocks;
                }
                Stage::Blocks => {
                    let Some((&at, &block)) = self.blocks.iter().next() else {
                        self.stage = Stage::Done;
                        continue;
                    };
                    let Some(data) = self.take(at, (512 + VHD_BLOCK) as usize)? else {
                        return Ok(());
                    };
                    self.blocks.remove(&at);
                    let start = block * VHD_BLOCK;
                    for (i, chunk) in data[512..].chunks(CHUNK).enumerate() {
                        let o = start + (i * CHUNK) as u64;
                        if o < self.vhd_size {
                            put(&mut self.content, o, chunk);
                        }
                    }
                }
                Stage::Done => {
                    // The trailing footer and anything after it.
                    self.at += self.buf.len() as u64;
                    self.buf.clear();
                    return Ok(());
                }
            }
        }
    }
}

/// The made-up content of a disk the mock never received: `count`
/// chunks spread over its `size` bytes.
pub(crate) fn made_up(uuid: &str, size: u64, count: u64) -> Content {
    let mut c = Content::new();
    let chunks = size / CHUNK as u64;
    if chunks == 0 {
        return c;
    }
    for i in 0..count.min(chunks) {
        let at = (i * chunks / count.max(1)) * CHUNK as u64;
        put(&mut c, at, &crate::xva::block_pattern(uuid, i, CHUNK));
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_vhds_read_back_in_any_pieces() {
        let size = 9 << 20;
        let content = made_up("u-1", size, 4);
        assert_eq!(content.len(), 4);
        let vhd: Vec<u8> = write_dynamic(size, &content).concat();
        for piece in [1usize, 511, 4096, 1 << 20, vhd.len()] {
            let mut r = Reader::new(true, size);
            for p in vhd.chunks(piece) {
                r.feed(p).unwrap();
            }
            r.finish().unwrap();
            assert_eq!(digest(&r.content), digest(&content), "pieces of {piece}");
        }
        // Too small a disk.
        let mut r = Reader::new(true, size - 1);
        assert!(r.feed(&vhd).is_err());
        // Cut short.
        let mut r = Reader::new(true, size);
        r.feed(&vhd[..vhd.len() / 2]).unwrap();
        assert!(r.finish().is_err());
    }

    #[test]
    fn raw_bytes_and_their_digest() {
        let mut bytes = vec![0u8; 3 * CHUNK + 100];
        bytes[CHUNK + 5] = 9;
        bytes[3 * CHUNK + 99] = 1;
        let mut r = Reader::new(false, bytes.len() as u64);
        for p in bytes.chunks(1000) {
            r.feed(p).unwrap();
        }
        r.finish().unwrap();
        assert_eq!(r.content.len(), 2);
        assert_eq!(digest(&r.content), content_digest(&bytes));
        let mut r = Reader::new(false, 10);
        r.feed(&bytes).unwrap_err();
    }
}
