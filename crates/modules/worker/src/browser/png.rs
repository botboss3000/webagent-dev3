//! PNG checks for private screenshots and the one-pixel reads behind `pixels`. The pixel
//! read inflates the capture's zlib stream with a small decoder written here, since the
//! worker has no compression dependency.
use super::Fault;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// Inflated output allowed for a pixel capture; a 1x1 image needs a few bytes.
const MAX_INFLATED: usize = 1 << 20;

fn be32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Verifies PNG bytes and dimensions before a screenshot is stored or served.
///
/// # Errors
/// A rejection when the bytes are not a PNG of at most `max` bytes inside the viewport
/// bounds.
pub fn metadata(data: &[u8], max: usize) -> Result<Value, Fault> {
    if !(24..=max).contains(&data.len()) || &data[..8] != SIGNATURE || &data[12..16] != b"IHDR" {
        return Err(Fault::rejected("Screenshot is not a bounded PNG"));
    }
    let (width, height) = (be32(data, 16).unwrap_or(0), be32(data, 20).unwrap_or(0));
    if !(1..=1600).contains(&width) || !(1..=1200).contains(&height) {
        return Err(Fault::rejected(
            "Screenshot dimensions exceed viewport bounds",
        ));
    }
    Ok(
        json!({"media_type":"image/png","bytes":data.len(),"width":width,"height":height,
        "sha256":hex::encode(Sha256::digest(data))}),
    )
}

/// Python's `struct.error` and `zlib.error` are both named `error`.
fn malformed() -> Fault {
    Fault::backend("error")
}

/// The first pixel of an 8-bit RGB or RGBA PNG. One pixel has no left or upper neighbour,
/// so every row filter leaves it unchanged.
///
/// # Errors
/// A rejection for another pixel format; a backend failure for malformed data.
pub fn pixel(data: &[u8]) -> Result<[u8; 3], Fault> {
    if data.len() < 26
        || &data[..8] != SIGNATURE
        || &data[12..16] != b"IHDR"
        || data[24] != 8
        || !matches!(data[25], 2 | 6)
    {
        return Err(Fault::rejected("Unsupported pixel capture"));
    }
    let mut position = 8;
    let mut idat = Vec::new();
    while position < data.len() {
        let length = be32(data, position).ok_or_else(malformed)? as usize;
        let kind = data.get(position + 4..position + 8).ok_or_else(malformed)?;
        if kind == b"IDAT" {
            let end = (position + 8 + length).min(data.len());
            idat.extend_from_slice(&data[position + 8..end]);
        }
        position += 12 + length;
    }
    let row = zlib(&idat).ok_or_else(malformed)?;
    row.get(1..4)
        .map(|rgb| [rgb[0], rgb[1], rgb[2]])
        .ok_or_else(|| Fault::backend("IndexError"))
}

/// Decompresses a zlib stream (RFC 1950 around RFC 1951), checking its Adler-32.
pub(crate) fn zlib(data: &[u8]) -> Option<Vec<u8>> {
    let (&cmf, &flg) = (data.first()?, data.get(1)?);
    if cmf & 0x0f != 8 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
        return None;
    }
    let mut bits = Bits {
        data,
        at: 2,
        buffer: 0,
        count: 0,
    };
    let output = inflate(&mut bits)?;
    let end = bits.at;
    let expected = be32(data, end)?;
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &output {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16 | a == expected).then_some(output)
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    buffer: u32,
    count: u32,
}

impl Bits<'_> {
    fn take(&mut self, need: u32) -> Option<u32> {
        while self.count < need {
            let byte = *self.data.get(self.at)?;
            self.at += 1;
            self.buffer |= u32::from(byte) << self.count;
            self.count += 8;
        }
        let value = self.buffer & ((1u32 << need) - 1);
        self.buffer = self.buffer.checked_shr(need).unwrap_or(0);
        self.count -= need;
        Some(value)
    }

    fn align(&mut self) {
        self.buffer = 0;
        self.count = 0;
    }
}

/// A canonical Huffman code: the number of codes per length and the symbols in order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Self> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            counts[usize::from(length)] += 1;
        }
        counts[0] = 0;
        let mut left = 1i32;
        for count in &counts[1..] {
            left = (left << 1) - i32::from(*count);
            if left < 0 {
                return None;
            }
        }
        let mut offsets = [0u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let slot = &mut offsets[usize::from(length)];
                symbols[usize::from(*slot)] = symbol as u16;
                *slot += 1;
            }
        }
        Some(Self { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for length in 1..16 {
            code |= bits.take(1)? as i32;
            let count = i32::from(self.counts[length]);
            if code - count < first {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn inflate(bits: &mut Bits<'_>) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.align();
                let header = bits.data.get(bits.at..bits.at + 4)?;
                let length = u16::from_le_bytes([header[0], header[1]]);
                if length != !u16::from_le_bytes([header[2], header[3]]) {
                    return None;
                }
                let start = bits.at + 4;
                output.extend_from_slice(bits.data.get(start..start + usize::from(length))?);
                bits.at = start + usize::from(length);
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let literals = Huffman::new(&lengths)?;
                let distances = Huffman::new(&[5u8; 30])?;
                codes(bits, &literals, &distances, &mut output)?;
            }
            2 => {
                let (literals, distances) = dynamic(bits)?;
                codes(bits, &literals, &distances, &mut output)?;
            }
            _ => return None,
        }
        if output.len() > MAX_INFLATED {
            return None;
        }
        if last {
            // The Adler-32 trailer starts at the next whole byte.
            bits.align();
            return Some(output);
        }
    }
}

fn dynamic(bits: &mut Bits<'_>) -> Option<(Huffman, Huffman)> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let literal_count = bits.take(5)? as usize + 257;
    let distance_count = bits.take(5)? as usize + 1;
    let code_count = bits.take(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return None;
    }
    let mut code_lengths = [0u8; 19];
    for &slot in &ORDER[..code_count] {
        code_lengths[slot] = bits.take(3)? as u8;
    }
    let lengths_code = Huffman::new(&code_lengths)?;
    let mut lengths = Vec::with_capacity(literal_count + distance_count);
    while lengths.len() < literal_count + distance_count {
        let symbol = lengths_code.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => (*lengths.last()?, 3 + bits.take(2)?),
            17 => (0, 3 + bits.take(3)?),
            18 => (0, 11 + bits.take(7)?),
            _ => return None,
        };
        for _ in 0..repeat {
            lengths.push(value);
        }
    }
    if lengths.len() != literal_count + distance_count || lengths[256] == 0 {
        return None;
    }
    Some((
        Huffman::new(&lengths[..literal_count])?,
        Huffman::new(&lengths[literal_count..])?,
    ))
}

fn codes(
    bits: &mut Bits<'_>,
    literals: &Huffman,
    distances: &Huffman,
    output: &mut Vec<u8>,
) -> Option<()> {
    loop {
        let symbol = usize::from(literals.decode(bits)?);
        match symbol {
            0..=255 => output.push(symbol as u8),
            256 => return Some(()),
            _ => {
                let index = symbol.checked_sub(257).filter(|index| *index < 29)?;
                let length = usize::from(LENGTH_BASE[index])
                    + bits.take(u32::from(LENGTH_EXTRA[index]))? as usize;
                let code = usize::from(distances.decode(bits)?);
                if code >= 30 {
                    return None;
                }
                let distance = usize::from(DISTANCE_BASE[code])
                    + bits.take(u32::from(DISTANCE_EXTRA[code]))? as usize;
                let start = output.len().checked_sub(distance)?;
                for offset in 0..length {
                    output.push(output[start + offset]);
                }
                if output.len() > MAX_INFLATED {
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The 1x1 PNG the Python tests used.
    pub(crate) const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aJ4cAAAAASUVORK5CYII=";

    fn chunk(kind: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = (body.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    /// A 1x1 RGBA PNG whose IDAT is `compressed`.
    pub(crate) fn one_pixel(compressed: &[u8]) -> Vec<u8> {
        let mut png = SIGNATURE.to_vec();
        png.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]));
        png.extend(chunk(b"IDAT", compressed));
        png.extend(chunk(b"IEND", b""));
        png
    }

    #[test]
    fn metadata_bounds_and_digest() {
        let png = crate::browser::base64::decode(PNG).expect("fixture");
        let meta = metadata(&png, 1 << 20).expect("valid");
        assert_eq!(
            (meta["width"].clone(), meta["height"].clone()),
            (json!(1), json!(1))
        );
        assert_eq!(meta["bytes"], json!(png.len()));
        assert!(metadata(&png[..23], 1 << 20).is_err());
        let mut wide = png.clone();
        wide[16..20].copy_from_slice(&1601u32.to_be_bytes());
        assert!(metadata(&wide, 1 << 20).is_err());
    }

    #[test]
    fn pixel_reads_python_zlib_streams() {
        // zlib.compress(b"\x04\x12\x34\x56\xff") from the Python test (fixed Huffman).
        let fixed = [
            0x78, 0x9c, 0x63, 0x11, 0x32, 0x09, 0xfb, 0x0f, 0x00, 0x02, 0xa8, 0x01, 0xa0,
        ];
        assert_eq!(zlib(&fixed), Some(vec![4, 0x12, 0x34, 0x56, 0xff]));
        assert_eq!(
            pixel(&one_pixel(&fixed)).expect("pixel"),
            [0x12, 0x34, 0x56]
        );
        // A stored block.
        let stored = [
            0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, 0x00, 0x01, 0x02, 0x03, 0xff, 0x01, 0x14,
            0x01, 0x06,
        ];
        assert_eq!(zlib(&stored), Some(vec![0, 1, 2, 3, 0xff]));
        // zlib.compress(data, 9) of 3900 generated bytes: dynamic Huffman codes.
        let dynamic = hex::decode(
            [
            "78daedd2d7570f001c86710a21b3412842c34aa2222223495466ca4a45222ac928918651c94a5484a2ec3d32",
            "22ab5064a6ac64b4c80851ae5cfcbee778fe03379df35e3f37efa79e4a0b4dedae3d4c0658d9d84f9aee317f",
            "d1f290b531db76a61c3a792133fbfed397ef2abefd566edaba9dae616f33cb1176e39d5d3d172e090e8fda12",
            "bf27edd8d9cb37ee3e2a7c53faf9673d45a6afc5d0510e9325b3316ed7bec3a724f3b1aaa681aa9a64c64c70",
            "9935d74732078e9fcbb8992399c62ddbe8e8f594cc6c6fffc0d5eb2473ed765efeabf792e9dccdd87cb0b564",
            "5646446f4dd82b99e7c5655faaeb4b66e0305b47a71992d99eb4ffc8e98b92f95edbb0997a7bc94c9ceae6e5",
            "bb543227ce5fb995fb5832adda76d4efd54f320b160785aedf24993b0f9ebdfef04932ddfbf41f3272ac64d6",
            "6c884d4c3e2899b7e55f7f293591ccf0d1e3a6cc9c2399dda947cf5cba2e993f8d9a6b74e8229969eef3fc96",
            "ad924cfad5ac7b4f5e4846ab938191e920c904ac088bdcbc43320f0b8a4a2a7f484671a66414674a4671a664",
            "14674a4671a66468621a4da4d384164d04d0c4439a30a189189aa8a0093b9a48a309159af0a0894c9ad0a589",
            "609a28a4090b9a88a3892a9a984013c769a2254d78d3c46d9ae846131134514c13c3682289266a69622a4d9c",
            "a789b634b198261ed0441f9ad84013e534319a265269a2114db8d3c4559ae844132b68a2802606d0c4369af8",
            "4613e369e2184db4a089f934914d138634114e136f6862284deca2891a9a70a1897334d18626fc69228f268c",
            "69229a26ca68c29626f6d344439a70a3892b34d191268268e2194df4a789589af84a13e368e2284d34a78979",
            "34914513063411461345346145133b69e2374d38d3c4599ad0a4894534719f267ad344144d94d2c4289ad847",
            "130d6862164d64d0840e4d04d2443e4d98d3c4569af842138e347184269ad184174ddca2097d9a08a589d734",
            "3184261269e2174d4ca1893334a141137e34718f268c682292264a68c2862652684299265c69e2324d68d3c4",
            "729a784a136634b185263ed384034d1ca609559a984b133769428f2656d3c42b9a184c130934514d134e3471",
            "9a26d469c297267269a2174daca7890f34319226926942892666d2c4259ae84013cb68e2094d98d2c4669aa8",
            "a4097b9a3844134d69c293266ed044579a08a1899734614913f134f1932626d3c4299a50a3091f9ac8a1899e",
            "34b18e26ded384f53f136eee1e75ab5bddfee3fe02e6340d29",
            ]
            .concat(),
        )
        .expect("hex");
        let data: Vec<u8> = (0..3000u32)
            .map(|i| ((i * 7 + i / 13) % 251) as u8)
            .chain(b"abcabcabc".repeat(100))
            .collect();
        assert_eq!(zlib(&dynamic), Some(data));
        let mut corrupt = fixed;
        corrupt[12] ^= 1;
        assert!(zlib(&corrupt).is_none());
        let png = crate::browser::base64::decode(PNG).expect("fixture");
        assert!(
            matches!(pixel(&png), Err(Fault::Rejected(_))),
            "grey is unsupported"
        );
    }
}
