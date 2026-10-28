//! Unreal Engine 5 IoStore containers (`.utoc` table of contents + `.ucas` data).

use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};

use super::reader::Reader;

const MAGIC: &[u8; 16] = b"-==--==--==--==-";
const HEADER_SIZE: usize = 144;
const FLAG_COMPRESSED: u8 = 1;
const FLAG_ENCRYPTED: u8 = 2;
const FLAG_SIGNED: u8 = 4;
const FLAG_INDEXED: u8 = 8;
// TOC versions that changed the layout we read.
const VERSION_DIRECTORY_INDEX: u8 = 2;
const VERSION_PERFECT_HASH: u8 = 4;
const VERSION_PERFECT_HASH_OVERFLOW: u8 = 5;

/// `EIoChunkType` values we care about (UE5 numbering).
pub mod chunk_type {
    pub const SHADER_CODE_LIBRARY: u8 = 8;
    pub const SHADER_CODE: u8 = 9;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkId(pub [u8; 12]);

impl ChunkId {
    pub fn kind(&self) -> u8 {
        self.0[11]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Chunk {
    pub id: ChunkId,
    /// Offset and length in the container's uncompressed address space.
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, Copy)]
struct Block {
    offset: u64,
    compressed: u32,
    uncompressed: u32,
    /// 0 = stored, otherwise 1-based index into `methods`.
    method: u8,
}

pub struct Toc {
    pub path: PathBuf,
    pub version: u8,
    pub chunks: Vec<Chunk>,
    /// File paths from the directory index, mapped to their chunk index.
    pub files: Vec<(String, usize)>,
    by_id: HashMap<ChunkId, usize>,
    blocks: Vec<Block>,
    methods: Vec<String>,
    block_size: u64,
    partition_size: u64,
    partitions: Vec<PathBuf>,
}

fn be40(b: &[u8]) -> u64 {
    b.iter().fold(0, |acc, &x| (acc << 8) | u64::from(x))
}

fn le(b: &[u8]) -> u64 {
    b.iter().rev().fold(0, |acc, &x| (acc << 8) | u64::from(x))
}

impl Toc {
    pub fn open(utoc: &Path) -> Result<Self> {
        let data = fs::read(utoc).with_context(|| format!("reading {}", utoc.display()))?;
        Self::parse(utoc, &data).with_context(|| format!("parsing {}", utoc.display()))
    }

    fn parse(utoc: &Path, data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        ensure!(r.bytes(16)? == MAGIC, "not an IoStore TOC");
        let version = r.u8()?;
        r.skip(3)?;
        let header_size = r.u32()? as usize;
        ensure!(header_size == HEADER_SIZE, "unexpected TOC header size {header_size}");
        let entry_count = r.u32()? as usize;
        let block_count = r.u32()? as usize;
        let block_entry_size = r.u32()?;
        ensure!(block_entry_size == 12, "unexpected compression block entry size {block_entry_size}");
        let method_count = r.u32()? as usize;
        let method_len = r.u32()? as usize;
        let block_size = u64::from(r.u32()?);
        let dir_index_size = r.u32()? as usize;
        let partition_count = r.u32()?.max(1) as usize;
        r.skip(8 + 16)?; // container id, encryption key guid
        let flags = r.u8()?;
        r.skip(3)?;
        let seed_count = r.u32()? as usize;
        let partition_size = r.u64()?;
        let overflow_count = r.u32()? as usize;
        r.skip(HEADER_SIZE - r.pos())?;
        if flags & FLAG_ENCRYPTED != 0 {
            bail!("container is encrypted (AES key required)");
        }

        let mut ids = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            ids.push(ChunkId(r.array()?));
        }
        let mut chunks = Vec::with_capacity(entry_count);
        for id in ids {
            let b = r.bytes(10)?;
            chunks.push(Chunk { id, offset: be40(&b[..5]), length: be40(&b[5..]) });
        }
        if version >= VERSION_PERFECT_HASH {
            r.skip(seed_count * 4)?;
        }
        if version >= VERSION_PERFECT_HASH_OVERFLOW {
            r.skip(overflow_count * 4)?;
        }
        let mut blocks = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            let b = r.bytes(12)?;
            blocks.push(Block {
                offset: le(&b[..5]),
                compressed: le(&b[5..8]) as u32,
                uncompressed: le(&b[8..11]) as u32,
                method: b[11],
            });
        }
        let mut methods = Vec::with_capacity(method_count);
        for _ in 0..method_count {
            let raw = r.bytes(method_len)?;
            let name = raw.split(|&c| c == 0).next().unwrap_or_default();
            methods.push(String::from_utf8_lossy(name).into_owned());
        }
        if flags & FLAG_SIGNED != 0 {
            let hash_size = r.u32()? as usize;
            r.skip(hash_size * 2 + 20 * block_count)?;
        }
        let mut files = Vec::new();
        if version >= VERSION_DIRECTORY_INDEX && flags & FLAG_INDEXED != 0 && dir_index_size > 0 {
            let index = r.bytes(dir_index_size)?;
            files = parse_directory_index(index).context("reading directory index")?;
        }
        if flags & FLAG_COMPRESSED == 0 {
            ensure!(blocks.iter().all(|b| b.method == 0), "uncompressed container with compressed blocks");
        }

        let base = utoc.with_extension("");
        let partitions = (0..partition_count)
            .map(|i| {
                let suffix = if i == 0 { String::new() } else { format!("_s{i}") };
                PathBuf::from(format!("{}{suffix}.ucas", base.display()))
            })
            .collect();
        let by_id = chunks.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        Ok(Self {
            path: utoc.to_path_buf(),
            version,
            chunks,
            files,
            by_id,
            blocks,
            methods,
            block_size,
            partition_size,
            partitions,
        })
    }

    pub fn chunk_index(&self, id: &ChunkId) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// Reads and decompresses one chunk.
    pub fn read_chunk(&self, index: usize, files: &mut UcasFiles) -> Result<Vec<u8>> {
        let chunk = *self.chunks.get(index).context("chunk index out of range")?;
        if chunk.length == 0 {
            return Ok(Vec::new());
        }
        let first = (chunk.offset / self.block_size) as usize;
        let last = ((chunk.offset + chunk.length - 1) / self.block_size) as usize;
        let mut out = Vec::with_capacity(((last - first + 1) as u64 * self.block_size) as usize);
        let mut scratch = Vec::new();
        for block in self.blocks.get(first..=last).context("chunk blocks out of range")? {
            let partition = if self.partition_size == u64::MAX { 0 } else { (block.offset / self.partition_size) as usize };
            let offset = if self.partition_size == u64::MAX { block.offset } else { block.offset % self.partition_size };
            let file = files.get(self.partitions.get(partition).context("block in missing partition")?)?;
            scratch.resize(block.compressed as usize, 0);
            file.read_exact_at(&mut scratch, offset).context("reading .ucas block")?;
            match block.method {
                0 => out.extend_from_slice(&scratch[..block.uncompressed as usize]),
                m => {
                    let method = self.methods.get(m as usize - 1).map(String::as_str).unwrap_or("?");
                    let start = out.len();
                    out.resize(start + block.uncompressed as usize, 0);
                    decompress(method, &scratch, &mut out[start..])?;
                }
            }
        }
        let skip = (chunk.offset % self.block_size) as usize;
        out.drain(..skip);
        out.truncate(chunk.length as usize);
        Ok(out)
    }
}

fn decompress(method: &str, input: &[u8], output: &mut [u8]) -> Result<()> {
    match method.to_ascii_lowercase().as_str() {
        "oodle" => {
            let n = oozextract::Extractor::new()
                .read_from_slice(input, output)
                .map_err(|e| anyhow::anyhow!("Oodle decompression failed: {e:?}"))?;
            ensure!(n == output.len(), "Oodle produced {n} of {} bytes", output.len());
            Ok(())
        }
        other => bail!("unsupported IoStore compression method '{other}'"),
    }
}

/// Open `.ucas` handles shared across chunk reads.
#[derive(Default)]
pub struct UcasFiles(HashMap<PathBuf, File>);

impl UcasFiles {
    fn get(&mut self, path: &Path) -> Result<&File> {
        if !self.0.contains_key(path) {
            let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
            self.0.insert(path.to_path_buf(), f);
        }
        Ok(&self.0[path])
    }
}

const NONE: u32 = u32::MAX;

/// `FIoDirectoryIndexResource`: mount point, directory tree, file list and string table.
fn parse_directory_index(data: &[u8]) -> Result<Vec<(String, usize)>> {
    let mut r = Reader::new(data);
    let mount = r.fstring()?;
    let n_dirs = r.count(16)?;
    let mut dirs = Vec::with_capacity(n_dirs);
    for _ in 0..n_dirs {
        dirs.push([r.u32()?, r.u32()?, r.u32()?, r.u32()?]); // name, first child, next sibling, first file
    }
    let n_files = r.count(12)?;
    let mut entries = Vec::with_capacity(n_files);
    for _ in 0..n_files {
        entries.push([r.u32()?, r.u32()?, r.u32()?]); // name, next file, chunk index
    }
    let n_strings = r.count(4)?;
    let mut strings = Vec::with_capacity(n_strings);
    for _ in 0..n_strings {
        strings.push(r.fstring()?);
    }
    let name = |i: u32| if i == NONE { "" } else { strings.get(i as usize).map_or("", String::as_str) };

    let mut out = Vec::with_capacity(n_files);
    // Iterative walk; (directory index, path so far). Guards against cycles in corrupt input.
    let mut stack = vec![(0u32, mount.trim_end_matches('/').to_string())];
    let mut visited = vec![false; n_dirs];
    while let Some((d, prefix)) = stack.pop() {
        let Some(&[dir_name, child, sibling, first_file]) = dirs.get(d as usize) else { continue };
        if std::mem::replace(&mut visited[d as usize], true) {
            continue;
        }
        if sibling != NONE {
            stack.push((sibling, prefix.clone()));
        }
        let path = if d == 0 || dir_name == NONE { prefix } else { format!("{prefix}/{}", name(dir_name)) };
        if child != NONE {
            stack.push((child, path.clone()));
        }
        let mut f = first_file;
        let mut guard = 0;
        while f != NONE && guard < n_files {
            let Some(&[file_name, next, chunk]) = entries.get(f as usize) else { break };
            out.push((format!("{path}/{}", name(file_name)), chunk as usize));
            f = next;
            guard += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fstring(out: &mut Vec<u8>, s: &str) {
        out.extend(((s.len() + 1) as i32).to_le_bytes());
        out.extend(s.as_bytes());
        out.push(0);
    }

    #[test]
    fn walks_directory_index() {
        // /Game/Shaders/{a.ushaderbytecode, b.upipelinecache}, /Game/x.uasset
        let mut d = Vec::new();
        fstring(&mut d, "../../../");
        let dirs: [[u32; 4]; 3] = [[NONE, 1, NONE, NONE], [0, 2, NONE, 2], [1, NONE, NONE, 0]];
        d.extend(3i32.to_le_bytes());
        dirs.iter().flatten().for_each(|v| d.extend(v.to_le_bytes()));
        let files: [[u32; 3]; 3] = [[2, 1, 10], [3, NONE, 11], [4, NONE, 12]];
        d.extend(3i32.to_le_bytes());
        files.iter().flatten().for_each(|v| d.extend(v.to_le_bytes()));
        d.extend(5i32.to_le_bytes());
        for s in ["Game", "Shaders", "a.ushaderbytecode", "b.upipelinecache", "x.uasset"] {
            fstring(&mut d, s);
        }
        let mut got = parse_directory_index(&d).unwrap();
        got.sort();
        assert_eq!(
            got,
            [
                ("../../../Game/Shaders/a.ushaderbytecode".to_string(), 10),
                ("../../../Game/Shaders/b.upipelinecache".to_string(), 11),
                ("../../../Game/x.uasset".to_string(), 12),
            ]
        );
    }

    #[test]
    fn packed_integers() {
        assert_eq!(be40(&[0, 0, 0, 1, 2]), 0x102);
        assert_eq!(le(&[1, 2, 0]), 0x201);
    }
}
