//! A streaming reader for the tar dialect xbps writes: POSIX ustar with pax extended headers.
//!
//! This is not a general tar reader, for two reasons. Tar libraries read long name records into
//! memory, so a tiny compressed file could claim gigabytes of file name. And the real extractor is
//! libarchive, so any construct where two readers might disagree about the entries is a way to show
//! this code one archive and the system another. Those constructs are refused as malformed rather
//! than interpreted: GNU long names, sparse files, base-256 numbers, and bodies on entries that
//! should not have one.

use std::io::{self, Read};

pub const BLOCK: usize = 512;
const MAX_PAX_BODY: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Regular,
    Hardlink,
    Symlink,
    CharDevice,
    BlockDevice,
    Directory,
    Fifo,
    /// Any other type flag. libarchive extracts unknown types as regular files.
    Unknown(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub path: String,
    pub link: String,
    /// False when the name or link was not valid UTF-8, in which case those fields are lossy.
    pub valid_utf8: bool,
    pub kind: Kind,
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
    pub size: u64,
    /// Pax keys that set extended attributes, ACLs or anything else this reader does not model.
    pub unknown_pax_keys: Vec<String>,
}

#[derive(Debug)]
pub enum TarError {
    /// The stream ended inside the archive.
    Truncated,
    Malformed(String),
    /// A feature that is deliberately not interpreted.
    Unsupported(String),
    Io(io::Error),
}

impl From<io::Error> for TarError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            TarError::Truncated
        } else {
            TarError::Io(error)
        }
    }
}

fn field(block: &[u8], start: usize, len: usize) -> &[u8] {
    let raw = &block[start..start + len];
    let end = raw.iter().position(|b| *b == 0).unwrap_or(len);
    &raw[..end]
}

fn octal(block: &[u8], start: usize, len: usize, name: &str) -> Result<u64, TarError> {
    let raw = &block[start..start + len];
    if raw[0] & 0x80 != 0 {
        return Err(TarError::Unsupported(format!("base-256 {name} field")));
    }
    let mut value: u64 = 0;
    let mut digits = raw.iter().skip_while(|b| **b == b' ').peekable();
    while let Some(byte) = digits.next_if(|b| (b'0'..=b'7').contains(*b)) {
        value = value
            .checked_mul(8)
            .and_then(|v| v.checked_add(u64::from(byte - b'0')))
            .ok_or_else(|| TarError::Malformed(format!("{name} field is too large")))?;
    }
    // Only terminators may follow the digits, or another reader might parse a different number.
    if digits.any(|b| *b != 0 && *b != b' ') {
        return Err(TarError::Malformed(format!("{name} field is not octal")));
    }
    Ok(value)
}

fn checksum_matches(block: &[u8]) -> Result<bool, TarError> {
    let stored = octal(block, 148, 8, "checksum")?;
    let sum: u64 = block
        .iter()
        .enumerate()
        .map(|(at, byte)| if (148..156).contains(&at) { u64::from(b' ') } else { u64::from(*byte) })
        .sum();
    Ok(sum == stored)
}

fn text(bytes: Vec<u8>, valid: &mut bool) -> String {
    String::from_utf8(bytes).unwrap_or_else(|error| {
        *valid = false;
        String::from_utf8_lossy(error.as_bytes()).into_owned()
    })
}

#[derive(Default)]
struct PaxOverrides {
    path: Option<Vec<u8>>,
    link: Option<Vec<u8>>,
    size: Option<u64>,
    uid: Option<u64>,
    gid: Option<u64>,
    unknown: Vec<String>,
}

fn pax_number(value: &[u8], key: &str) -> Result<u64, TarError> {
    std::str::from_utf8(value)
        .ok()
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| TarError::Malformed(format!("pax {key} is not a number")))
}

fn parse_pax(body: &[u8]) -> Result<PaxOverrides, TarError> {
    let mut overrides = PaxOverrides::default();
    let mut rest = body;
    while !rest.is_empty() {
        let malformed = || TarError::Malformed("bad pax record".to_string());
        let space = rest.iter().position(|b| *b == b' ').ok_or_else(malformed)?;
        let digits = std::str::from_utf8(&rest[..space]).map_err(|_| malformed())?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(malformed());
        }
        let length: usize = digits.parse().map_err(|_| malformed())?;
        if length <= space + 1 || length > rest.len() || rest[length - 1] != b'\n' {
            return Err(malformed());
        }
        let record = &rest[space + 1..length - 1];
        rest = &rest[length..];
        let equals = record.iter().position(|b| *b == b'=').ok_or_else(malformed)?;
        let (key, value) = (&record[..equals], &record[equals + 1..]);
        let key = String::from_utf8_lossy(key);
        // NUL would end the name for a C reader but not for this one.
        if value.contains(&0) {
            return Err(malformed());
        }
        match key.as_ref() {
            "path" => overrides.path = Some(value.to_vec()),
            "linkpath" => overrides.link = Some(value.to_vec()),
            "size" => overrides.size = Some(pax_number(value, "size")?),
            "uid" => overrides.uid = Some(pax_number(value, "uid")?),
            "gid" => overrides.gid = Some(pax_number(value, "gid")?),
            "mtime" | "atime" | "ctime" | "uname" | "gname" | "comment" => {}
            other => overrides.unknown.push(other.to_string()),
        }
    }
    Ok(overrides)
}

/// Reads one header at a time. After each, the caller consumes the body with [`Self::read_body`]
/// or [`Self::skip_body`], and the padding is handled here.
pub struct TarReader<R> {
    reader: R,
    /// Body bytes and padding of the current entry not yet consumed.
    body_left: u64,
    padding_left: u64,
}

impl<R: Read> TarReader<R> {
    pub fn new(reader: R) -> Self {
        TarReader { reader, body_left: 0, padding_left: 0 }
    }

    fn set_body(&mut self, size: u64) {
        self.body_left = size;
        self.padding_left = (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
    }

    /// Discards the rest of the current body and its padding, using a fixed amount of memory.
    pub fn skip_body(&mut self) -> Result<(), TarError> {
        let mut left = self.body_left.saturating_add(self.padding_left);
        self.body_left = 0;
        self.padding_left = 0;
        let mut buffer = [0u8; 16 * 1024];
        while left > 0 {
            let want = left.min(buffer.len() as u64) as usize;
            let got = self.reader.read(&mut buffer[..want])?;
            if got == 0 {
                return Err(TarError::Truncated);
            }
            left -= got as u64;
        }
        Ok(())
    }

    /// Reads the current body into memory. The caller checks its size first.
    pub fn read_body(&mut self) -> Result<Vec<u8>, TarError> {
        let size = self.body_left;
        // Grows as data actually arrives, so a false size in a header allocates nothing.
        let mut body = Vec::new();
        (&mut self.reader).take(size).read_to_end(&mut body)?;
        if (body.len() as u64) < size {
            return Err(TarError::Truncated);
        }
        self.body_left = 0;
        self.skip_body()?;
        Ok(body)
    }

    fn read_block(&mut self) -> Result<Option<[u8; BLOCK]>, TarError> {
        let mut block = [0u8; BLOCK];
        let mut filled = 0;
        while filled < BLOCK {
            match self.reader.read(&mut block[filled..])? {
                0 if filled == 0 => return Ok(None),
                0 => return Err(TarError::Truncated),
                got => filled += got,
            }
        }
        Ok(Some(block))
    }

    /// Consumes everything after the end marker. Anything other than zero padding could be entries
    /// that a more forgiving reader would still extract.
    fn only_zeros_remain(&mut self) -> Result<bool, TarError> {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let got = self.reader.read(&mut buffer)?;
            if got == 0 {
                return Ok(true);
            }
            if buffer[..got].iter().any(|b| *b != 0) {
                return Ok(false);
            }
        }
    }

    /// Returns the next entry, or `None` at the end of the archive. `Err` is always fatal, because
    /// a reader that has lost its place cannot say what the rest of the stream means.
    pub fn next_header(&mut self) -> Result<Option<Header>, TarError> {
        self.skip_body()?;
        let mut overrides: Option<PaxOverrides> = None;
        loop {
            let Some(block) = self.read_block()? else {
                return Err(TarError::Truncated);
            };
            if block.iter().all(|b| *b == 0) {
                if overrides.is_some() {
                    return Err(TarError::Malformed("pax header with no entry".to_string()));
                }
                if !self.only_zeros_remain()? {
                    return Err(TarError::Malformed("data after the end of the archive".to_string()));
                }
                return Ok(None);
            }
            if !checksum_matches(&block)? {
                return Err(TarError::Malformed("bad header checksum".to_string()));
            }
            if &block[257..263] != b"ustar\0" || &block[263..265] != b"00" {
                return Err(TarError::Unsupported("archive is not POSIX ustar".to_string()));
            }
            let type_flag = block[156];
            let size = octal(&block, 124, 12, "size")?;
            match type_flag {
                b'x' => {
                    if overrides.is_some() {
                        return Err(TarError::Malformed("two pax headers for one entry".to_string()));
                    }
                    if size > MAX_PAX_BODY {
                        return Err(TarError::Malformed("pax header is too large".to_string()));
                    }
                    self.set_body(size);
                    overrides = Some(parse_pax(&self.read_body()?)?);
                }
                b'g' => return Err(TarError::Unsupported("pax global header".to_string())),
                b'L' | b'K' => return Err(TarError::Unsupported("GNU long name".to_string())),
                b'S' => return Err(TarError::Unsupported("GNU sparse file".to_string())),
                _ => return self.entry(&block, type_flag, size, overrides.take().unwrap_or_default()).map(Some),
            }
        }
    }

    fn entry(
        &mut self,
        block: &[u8; BLOCK],
        type_flag: u8,
        size: u64,
        overrides: PaxOverrides,
    ) -> Result<Header, TarError> {
        let kind = match type_flag {
            b'0' | 0 => Kind::Regular,
            b'1' => Kind::Hardlink,
            b'2' => Kind::Symlink,
            b'3' => Kind::CharDevice,
            b'4' => Kind::BlockDevice,
            b'5' => Kind::Directory,
            b'6' => Kind::Fifo,
            other => Kind::Unknown(other),
        };
        let size = overrides.size.unwrap_or(size);
        // Readers disagree on whether these types have a body, which would desynchronize them.
        if size != 0 && !matches!(kind, Kind::Regular | Kind::Unknown(_)) {
            return Err(TarError::Malformed("a non-file entry has a body".to_string()));
        }

        let name = field(block, 0, 100);
        let prefix = field(block, 345, 155);
        let mut path_bytes = Vec::with_capacity(prefix.len() + name.len() + 1);
        if !prefix.is_empty() {
            path_bytes.extend_from_slice(prefix);
            path_bytes.push(b'/');
        }
        path_bytes.extend_from_slice(name);

        let mut valid_utf8 = true;
        let path = text(overrides.path.unwrap_or(path_bytes), &mut valid_utf8);
        let link = text(overrides.link.unwrap_or_else(|| field(block, 157, 100).to_vec()), &mut valid_utf8);

        self.set_body(size);
        Ok(Header {
            path,
            link,
            valid_utf8,
            kind,
            mode: octal(block, 100, 8, "mode")? as u32,
            uid: match overrides.uid {
                Some(uid) => uid,
                None => octal(block, 108, 8, "uid")?,
            },
            gid: match overrides.gid {
                Some(gid) => gid,
                None => octal(block, 116, 8, "gid")?,
            },
            size,
            unknown_pax_keys: overrides.unknown,
        })
    }
}
