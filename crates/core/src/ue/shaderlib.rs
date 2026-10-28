//! UE5 IoStore shader libraries (`ShaderArchive-*.ushaderbytecode`): the header lives in
//! a `ShaderCodeLibrary` chunk, the code in `ShaderCode` chunks holding groups of shaders
//! that are compressed together.

use anyhow::{ensure, Context, Result};

use super::iostore::{ChunkId, Toc, UcasFiles};
use super::reader::Reader;

/// `EShaderFrequency` (UE5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Frequency {
    Vertex,
    Mesh,
    Amplification,
    Pixel,
    Geometry,
    Compute,
    RayTracing,
    Other(u8),
}

impl Frequency {
    fn from_bits(v: u8) -> Self {
        match v {
            0 => Frequency::Vertex,
            1 => Frequency::Mesh,
            2 => Frequency::Amplification,
            3 => Frequency::Pixel,
            4 => Frequency::Geometry,
            5 => Frequency::Compute,
            6..=10 => Frequency::RayTracing,
            other => Frequency::Other(other),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ShaderEntry {
    pub frequency: Frequency,
    pub group: u32,
    pub offset_in_group: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Group {
    pub chunk: ChunkId,
    pub indices_offset: u32,
    pub num_shaders: u32,
    pub uncompressed_size: u32,
    pub compressed_size: u32,
}

pub struct ShaderLibrary {
    pub shader_hashes: Vec<[u8; 20]>,
    pub shaders: Vec<ShaderEntry>,
    pub groups: Vec<Group>,
    indices: Vec<u32>,
}

/// One shader, as stored by the RHI (bytecode plus Unreal's own header/trailer).
pub struct Shader {
    pub index: usize,
    pub frequency: Frequency,
    pub code: Vec<u8>,
}

const SUPPORTED_VERSION: u32 = 1;

impl ShaderLibrary {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.u32()?;
        ensure!(version == SUPPORTED_VERSION, "unsupported shader library version {version}");
        let n = r.count(20)?;
        r.skip(n * 20)?; // shader map hashes
        let n = r.count(20)?;
        let mut shader_hashes = Vec::with_capacity(n);
        for _ in 0..n {
            shader_hashes.push(r.array()?);
        }
        let n = r.count(12)?;
        let mut group_ids = Vec::with_capacity(n);
        for _ in 0..n {
            group_ids.push(ChunkId(r.array()?));
        }
        let n = r.count(8)?;
        r.skip(n * 8)?; // shader map entries
        let n = r.count(8)?;
        let mut shaders = Vec::with_capacity(n);
        for _ in 0..n {
            // Bitfields, low to high: frequency (4), group index (30), offset in group (30).
            let packed = r.u64()?;
            shaders.push(ShaderEntry {
                frequency: Frequency::from_bits((packed & 0xF) as u8),
                group: ((packed >> 4) & 0x3FFF_FFFF) as u32,
                offset_in_group: ((packed >> 34) & 0x3FFF_FFFF) as u32,
            });
        }
        let n = r.count(16)?;
        ensure!(n == group_ids.len(), "group table mismatch ({n} entries, {} ids)", group_ids.len());
        let mut groups = Vec::with_capacity(n);
        for chunk in group_ids {
            groups.push(Group {
                chunk,
                indices_offset: r.u32()?,
                num_shaders: r.u32()?,
                uncompressed_size: r.u32()?,
                compressed_size: r.u32()?,
            });
        }
        let n = r.count(4)?;
        let mut indices = Vec::with_capacity(n);
        for _ in 0..n {
            indices.push(r.u32()?);
        }
        ensure!(shader_hashes.len() == shaders.len(), "shader hash/entry count mismatch");
        Ok(Self { shader_hashes, shaders, groups, indices })
    }

    /// Reads one group and splits it into its shaders.
    pub fn read_group(&self, group: usize, toc: &Toc, files: &mut UcasFiles) -> Result<Vec<Shader>> {
        let g = self.groups.get(group).context("group index out of range")?;
        let chunk = toc.chunk_index(&g.chunk).context("shader group chunk missing from container")?;
        let stored = toc.read_chunk(chunk, files)?;
        let data = if g.compressed_size < g.uncompressed_size {
            let mut out = vec![0u8; g.uncompressed_size as usize];
            let n = oozextract::Extractor::new()
                .read_from_slice(&stored, &mut out)
                .map_err(|e| anyhow::anyhow!("decompressing shader group {group}: {e:?}"))?;
            ensure!(n == out.len(), "shader group {group}: got {n} of {} bytes", out.len());
            out
        } else {
            stored
        };

        let start = g.indices_offset as usize;
        let members = self
            .indices
            .get(start..start + g.num_shaders as usize)
            .context("group shader indices out of range")?;
        let mut members: Vec<(usize, ShaderEntry)> = members
            .iter()
            .map(|&i| self.shaders.get(i as usize).map(|e| (i as usize, *e)))
            .collect::<Option<_>>()
            .context("shader index out of range")?;
        // Shaders are laid out back to back; each one ends where the next begins.
        members.sort_by_key(|(_, e)| e.offset_in_group);
        let mut out = Vec::with_capacity(members.len());
        for (k, (index, entry)) in members.iter().enumerate() {
            let begin = entry.offset_in_group as usize;
            let end = members.get(k + 1).map_or(data.len(), |(_, next)| next.offset_in_group as usize);
            let code = data.get(begin..end).context("shader extends past its group")?.to_vec();
            out.push(Shader { index: *index, frequency: entry.frequency, code });
        }
        Ok(out)
    }
}
