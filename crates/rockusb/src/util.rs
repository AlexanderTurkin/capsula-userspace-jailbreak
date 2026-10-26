//! Checksums and ciphers bit-for-bit compatible with xrock
//! (crc16.c / crc32.c / rc4.c).

/// CRC16-CCITT (poly 0x1021, MSB-first, no reflection) — as `crc16_sum`
/// in xrock: used for the MaskROM upload (appended big-endian to the
/// blob) and for idblocks.
pub fn crc16_xrock(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC32, MSB-first with poly 0x04C10DB7 (as `crc32_sum` / the table in
/// xrock; the caller passes the init value, 0 for the BOOT container).
/// NOTE: this is NOT the standard MPEG-2 polynomial 0x04C11DB7 — a
/// Rockchip legacy quirk, verified against the crc32.c table.
pub fn crc32_xrock(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_0DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// RC4 (as rc4.c in xrock). Applied to a data stream as a whole (the
/// continuing i/j state is never needed — xrock re-keys per block).
pub struct Rc4 {
    s: [u8; 256],
    i: usize,
    j: usize,
}

impl Rc4 {
    /// The Rockchip MaskROM/idblock key (the only one seen in the wild).
    pub const ROCKCHIP_KEY: [u8; 16] =
        [124, 78, 3, 4, 85, 5, 9, 7, 45, 44, 123, 56, 23, 13, 23, 17];

    pub fn new(key: &[u8]) -> Self {
        let mut s = [0u8; 256];
        for (i, v) in s.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut j: usize = 0;
        for i in 0..256 {
            j = (j + s[i] as usize + key[i % key.len()] as usize) & 0xff;
            s.swap(i, j);
        }
        Rc4 { s, i: 0, j: 0 }
    }

    /// Encrypt/decrypt the buffer in place (symmetric).
    pub fn apply(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            self.i = (self.i + 1) & 0xff;
            self.j = (self.j + self.s[self.i] as usize) & 0xff;
            self.s.swap(self.i, self.j);
            *b ^= self.s[(self.s[self.i] as usize + self.s[self.j] as usize) & 0xff];
        }
    }

    /// Encrypt a copy (handy for immutable inputs).
    pub fn encrypt_copy(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        Rc4::new(key).apply(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-checked against the xrock crc16 table (crc16.c).
    #[test]
    fn crc16_matches_xrock_table() {
        // crc16_table[1] = 0x1021: crc16(0, &[1]) == 0x1021
        assert_eq!(crc16_xrock(0x0000, &[0x01]), 0x1021);
        // crc16_table[2] = 0x2042
        assert_eq!(crc16_xrock(0x0000, &[0x02]), 0x2042);
        // init 0xffff over "123456789" = 0x29B1 (CCITT-FALSE)
        assert_eq!(crc16_xrock(0xffff, b"123456789"), 0x29B1);
    }

    #[test]
    fn crc32_xrock_known_values() {
        // xrock table: crc32_table[0]=0, [1]=0x04C10DB7 -> crc32(0,&[0,0,0,1])
        assert_eq!(crc32_xrock(0, &[0x00, 0x00, 0x00, 0x01]), 0x04C1_0DB7);
        // [2]=0x09821B6E, [3]=0x0D4316D9 (poly 0x04C10DB7 check)
        assert_eq!(crc32_xrock(0, &[0x00, 0x00, 0x00, 0x02]), 0x0982_1B6E);
        assert_eq!(crc32_xrock(0, &[0x00, 0x00, 0x00, 0x03]), 0x0D43_16D9);
        // empty input — identity
        assert_eq!(crc32_xrock(0x1234_5678, &[]), 0x1234_5678);
    }

    #[test]
    fn rc4_roundtrip_and_vector() {
        // RC4 with key "Key" over "Plaintext" = the classic vector (BBF316E8...)
        let ct = Rc4::encrypt_copy(b"Key", b"Plaintext");
        assert_eq!(
            ct,
            vec![0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
        let mut pt = ct;
        Rc4::new(b"Key").apply(&mut pt);
        assert_eq!(pt, b"Plaintext");
    }
}
