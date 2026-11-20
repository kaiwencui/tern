//! A minimal PNG encoder: 8-bit RGBA, no filtering, zlib stream made of
//! stored (uncompressed) deflate blocks. Screenshots and test images only
//! need to be correct, not small.

const CRC_TABLE: [u32; 256] = crc_table();

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

/// CRC-32 as PNG chunks use it (ISO 3309, reflected, initial and final xor of all ones).
pub(crate) fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for part in parts {
        for &b in *part {
            c = CRC_TABLE[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xffff_ffff
}

/// Adler-32, the zlib stream checksum.
pub(crate) fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 is the largest run that cannot overflow u32 before the modulo.
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    b << 16 | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Wraps raw bytes in a zlib stream of stored deflate blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    const MAX_BLOCK: usize = 65535;
    let blocks = raw.len().div_ceil(MAX_BLOCK).max(1);
    let mut out = Vec::with_capacity(raw.len() + blocks * 5 + 6);
    // CMF 0x78: deflate with a 32K window; FLG 0x01 makes the header a multiple of 31.
    out.extend_from_slice(&[0x78, 0x01]);
    let mut rest = raw;
    loop {
        let n = rest.len().min(MAX_BLOCK);
        let last = n == rest.len();
        out.push(last as u8);
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(&rest[..n]);
        rest = &rest[n..];
        if last {
            break;
        }
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// Encodes width x height RGBA pixels, row by row from the top, as a PNG file.
pub(crate) fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = width as usize * 4;
    assert_eq!(
        rgba.len(),
        stride * height as usize,
        "pixel buffer does not match the image size"
    );
    // Each scanline starts with its filter type; 0 means none.
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgba.chunks_exact(stride.max(1)).take(height as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    if stride == 0 {
        raw.resize(height as usize, 0);
    }

    let mut out = Vec::with_capacity(raw.len() + 128);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bits per channel
    ihdr[9] = 6; // RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Decodes what [encode] writes (stored blocks, filter 0), checking every
    /// checksum on the way. Returns (width, height, rgba).
    pub(crate) fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut at = 8;
        let (mut w, mut h) = (0, 0);
        let mut idat = Vec::new();
        let mut saw_end = false;
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = &png[at + 4..at + 8];
            let data = &png[at + 8..at + 8 + len];
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&[kind, data]), "chunk CRC");
            match kind {
                b"IHDR" => {
                    w = u32::from_be_bytes(data[0..4].try_into().unwrap());
                    h = u32::from_be_bytes(data[4..8].try_into().unwrap());
                    assert_eq!(&data[8..], &[8, 6, 0, 0, 0]);
                }
                b"IDAT" => idat.extend_from_slice(data),
                b"IEND" => saw_end = true,
                _ => panic!("unexpected chunk"),
            }
            at += 12 + len;
        }
        assert!(saw_end);
        assert_eq!(
            (idat[0] as u32 * 256 + idat[1] as u32) % 31,
            0,
            "zlib header check"
        );
        let mut raw = Vec::new();
        let mut p = 2;
        loop {
            let header = idat[p];
            assert_eq!(header & 0b110, 0, "only stored blocks");
            let n = u16::from_le_bytes([idat[p + 1], idat[p + 2]]);
            let nn = u16::from_le_bytes([idat[p + 3], idat[p + 4]]);
            assert_eq!(n, !nn);
            raw.extend_from_slice(&idat[p + 5..p + 5 + n as usize]);
            p += 5 + n as usize;
            if header & 1 == 1 {
                break;
            }
        }
        let adler = u32::from_be_bytes(idat[p..p + 4].try_into().unwrap());
        assert_eq!(adler, adler32(&raw));
        let stride = w as usize * 4;
        let mut rgba = Vec::new();
        for row in raw.chunks_exact(stride + 1) {
            assert_eq!(row[0], 0);
            rgba.extend_from_slice(&row[1..]);
        }
        assert_eq!(rgba.len(), stride * h as usize);
        (w, h, rgba)
    }

    #[test]
    fn known_checksums() {
        assert_eq!(crc32(&[b"123456789"]), 0xcbf4_3926);
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        assert_eq!(adler32(&[]), 1);
    }

    #[test]
    fn adler_survives_long_input() {
        // Reference computed with the plain definition, modulo every step.
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 + 3) as u8).collect();
        let (mut a, mut b) = (1u64, 0u64);
        for &x in &data {
            a = (a + x as u64) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(adler32(&data), (b << 16 | a) as u32);
    }

    #[test]
    fn round_trip_small() {
        let rgba: Vec<u8> = (0..3 * 2 * 4).map(|i| i as u8 * 9).collect();
        let png = encode(3, 2, &rgba);
        assert_eq!(decode(&png), (3, 2, rgba));
    }

    #[test]
    fn round_trip_spans_several_stored_blocks() {
        let (w, h) = (300u32, 200u32);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let png = encode(w, h, &rgba);
        assert_eq!(decode(&png), (w, h, rgba));
    }

    #[test]
    fn empty_image() {
        let png = encode(0, 0, &[]);
        let (w, h, rgba) = decode(&png);
        assert_eq!((w, h, rgba.len()), (0, 0, 0));
    }
}
