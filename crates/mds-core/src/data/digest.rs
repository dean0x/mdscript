//! SHA-256 (FIPS 180-4) and HMAC-SHA-256 (RFC 2104).
//!
//! Own code, so the core and the WASM build need no hash crate. The vectors of FIPS 180-4
//! and RFC 4231 in `digest_tests.rs` prove it. The block loop is bounded by the input
//! length and does not allocate.

/// The bytes of one SHA-256 block.
const BLOCK: usize = 64;
/// The block offset where the 64-bit message length starts in the last block.
const LENGTH_AT: usize = BLOCK - 8;

const INITIAL: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

const ROUND: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// The SHA-256 digest of the concatenation of `parts`.
#[must_use]
pub(crate) fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut state = Sha256::new();
    for part in parts {
        state.update(part);
    }
    state.finish()
}

/// The HMAC-SHA-256 of the concatenation of `parts` under `key`. A key longer than one
/// block is hashed first, as RFC 2104 says.
#[must_use]
pub(crate) fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    let hashed;
    let key = if key.len() > BLOCK {
        hashed = sha256(&[key]);
        &hashed[..]
    } else {
        key
    };
    for (slot, byte) in block.iter_mut().zip(key) {
        *slot = *byte;
    }
    let mut inner = Sha256::new();
    inner.update(&block.map(|byte| byte ^ 0x36));
    for part in parts {
        inner.update(part);
    }
    let inner = inner.finish();
    let mut outer = Sha256::new();
    outer.update(&block.map(|byte| byte ^ 0x5c));
    outer.update(&inner);
    outer.finish()
}

/// The lower-case hex text of a digest.
#[must_use]
pub(crate) fn hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

struct Sha256 {
    state: [u32; 8],
    /// The bytes of the block that is not full yet.
    pending: [u8; BLOCK],
    /// How many bytes of `pending` are in use, always below `BLOCK`.
    filled: usize,
    /// The message length in bytes, modulo 2^64.
    length: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: INITIAL,
            pending: [0; BLOCK],
            filled: 0,
            length: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        let mut data = data;
        if self.filled > 0 {
            let mut copied = 0;
            for (slot, byte) in self.pending.iter_mut().skip(self.filled).zip(data) {
                *slot = *byte;
                copied += 1;
            }
            self.filled += copied;
            data = data.get(copied..).unwrap_or_default();
            if self.filled < BLOCK {
                return;
            }
            compress(&mut self.state, &self.pending);
            self.filled = 0;
        }
        let (blocks, tail) = data.as_chunks::<BLOCK>();
        for block in blocks {
            compress(&mut self.state, block);
        }
        for (slot, byte) in self.pending.iter_mut().zip(tail) {
            *slot = *byte;
        }
        self.filled = tail.len();
    }

    fn finish(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8).to_be_bytes();
        let mut last = self.pending;
        for (index, slot) in last.iter_mut().enumerate().skip(self.filled) {
            *slot = if index == self.filled { 0x80 } else { 0 };
        }
        // The end mark and the 8 length bytes do not fit after `LENGTH_AT`: pad this
        // block and put the length in one more.
        if self.filled >= LENGTH_AT {
            compress(&mut self.state, &last);
            last = [0; BLOCK];
        }
        for (slot, byte) in last.iter_mut().skip(LENGTH_AT).zip(bits) {
            *slot = byte;
        }
        compress(&mut self.state, &last);
        let mut digest = [0u8; 32];
        for (bytes, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *bytes = word.to_be_bytes();
        }
        digest
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; BLOCK]) {
    let mut schedule = [0u32; 64];
    for (word, bytes) in schedule.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    // Each index stays inside the 64 words by the range of the loop.
    for t in 16..64 {
        let w15 = schedule[t - 15];
        let w2 = schedule[t - 2];
        let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
        let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
        schedule[t] = schedule[t - 16]
            .wrapping_add(s0)
            .wrapping_add(schedule[t - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (round, word) in ROUND.iter().zip(schedule) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(choice)
            .wrapping_add(*round)
            .wrapping_add(word);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(majority);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *word = word.wrapping_add(add);
    }
}
