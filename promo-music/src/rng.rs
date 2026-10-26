//! Seeded standard normals: xoshiro256** seeded by SplitMix64, Box–Muller.
//! The same seed gives the same layer on every host and backend (the noise
//! is drawn on the CPU). It is not PyTorch's generator: the parity tests
//! feed the reference's dumped noise instead.

pub struct Normal {
    s: [u64; 4],
    spare: Option<f32>,
}

impl Normal {
    pub fn new(seed: u64) -> Normal {
        let mut x = seed;
        let mut next = || {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        Normal {
            s: [next(), next(), next(), next()],
            spare: None,
        }
    }

    fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let r = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        r
    }

    /// Uniform in (0, 1].
    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }

    pub fn sample(&mut self) -> f32 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let r = (-2.0 * self.uniform().ln()).sqrt();
        let a = std::f64::consts::TAU * self.uniform();
        self.spare = Some((r * a.sin()) as f32);
        (r * a.cos()) as f32
    }

    pub fn fill(&mut self, n: usize) -> Vec<f32> {
        (0..n).map(|_| self.sample()).collect()
    }
}
