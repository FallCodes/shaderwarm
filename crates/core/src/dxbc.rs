//! DXBC containers: the wrapper around both SM4/5 bytecode and SM6 DXIL.

/// Fixed part of a container: magic, 16-byte checksum, version, total size, part count.
const HEADER: usize = 32;
const MAX_PARTS: u32 = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container<'a> {
    pub bytes: &'a [u8],
    pub parts: Vec<([u8; 4], &'a [u8])>,
}

impl<'a> Container<'a> {
    /// Parses a container at the start of `data`, checking its size and part table.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER || &data[..4] != b"DXBC" {
            return None;
        }
        let u32_at = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let total = u32_at(24)? as usize;
        let count = u32_at(28)?;
        if total < HEADER || total > data.len() || count == 0 || count > MAX_PARTS {
            return None;
        }
        let bytes = &data[..total];
        let mut parts = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let off = u32_at(HEADER + i * 4)? as usize;
            let tag: [u8; 4] = bytes.get(off..off + 4)?.try_into().ok()?;
            let len = u32_at(off + 4)? as usize;
            parts.push((tag, bytes.get(off + 8..off + 8 + len)?));
        }
        Some(Self { bytes, parts })
    }

    pub fn part(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        self.parts.iter().find(|(t, _)| t == tag).map(|(_, d)| *d)
    }

    /// SM6 containers carry a `DXIL` part; SM4/5 ones `SHEX`/`SHDR`.
    pub fn is_dxil(&self) -> bool {
        self.part(b"DXIL").is_some()
    }

    /// Embedded root signature, which lets a D3D12 compute PSO be built without guessing.
    pub fn root_signature(&self) -> Option<&'a [u8]> {
        self.part(b"RTS0")
    }
}

/// Every valid container inside `data` (engine headers and trailers are skipped over).
pub fn find_all(data: &[u8]) -> Vec<Container<'_>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = memfind(&data[pos..], b"DXBC") {
        let at = pos + i;
        match Container::parse(&data[at..]) {
            Some(c) => {
                pos = at + c.bytes.len();
                out.push(c);
            }
            None => pos = at + 4,
        }
    }
    out
}

fn memfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a container with the given parts (checksum left zero).
    pub fn container(parts: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let table = HEADER + parts.len() * 4;
        let mut body = Vec::new();
        let mut offsets = Vec::new();
        for (tag, data) in parts {
            offsets.push((table + body.len()) as u32);
            body.extend_from_slice(*tag);
            body.extend((data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
        }
        let mut out = b"DXBC".to_vec();
        out.extend([0u8; 16]);
        out.extend(1u32.to_le_bytes());
        out.extend(((table + body.len()) as u32).to_le_bytes());
        out.extend((parts.len() as u32).to_le_bytes());
        offsets.iter().for_each(|o| out.extend(o.to_le_bytes()));
        out.extend(body);
        out
    }

    #[test]
    fn parses_parts_and_skips_garbage() {
        let dxil = container(&[(b"DXIL", b"abcd"), (b"RTS0", b"rs")]);
        let sm5 = container(&[(b"SHEX", b"xy")]);
        let mut blob = b"UEheaderDXBCjunk".to_vec();
        blob.extend(&dxil);
        blob.extend(b"tail");
        blob.extend(&sm5);

        let found = find_all(&blob);
        assert_eq!(found.len(), 2);
        assert!(found[0].is_dxil());
        assert_eq!(found[0].root_signature(), Some(&b"rs"[..]));
        assert!(!found[1].is_dxil());
        assert_eq!(found[1].bytes, &sm5[..]);
    }

    #[test]
    fn rejects_truncated_container() {
        let c = container(&[(b"DXIL", b"abcd")]);
        assert!(Container::parse(&c[..c.len() - 1]).is_none());
    }
}
