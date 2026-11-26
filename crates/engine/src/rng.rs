//! The game's single shared LCG and its three shuffles (F2.4).

/// `Rand_LCG` 0x4104a0: MSVC `rand()` constants on the game's own state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lcg {
    state: u32,
}

impl Lcg {
    pub const fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    /// Raw state, for save/replay diagnostics.
    pub const fn state(&self) -> u32 {
        self.state
    }

    /// Next value in 0..=0x7fff.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(214_013).wrapping_add(2_531_011);
        (self.state >> 16) & 0x7fff
    }

    /// `ShuffleSwap1000` 0x410290: 1000 x swap a[rand % n], a[rand % n].
    /// An empty slice consumes nothing (the original would divide by zero).
    pub fn shuffle_swap1000(&mut self, a: &mut [i32]) {
        swap1000(self, a);
    }

    /// `ShuffleDraw` 0x4102d0 on `a[..n]`: if `base` is set and >= 0, fill a[i] = base + i first;
    /// then n draws, each taking the (rand % remaining)-th element not yet taken, and
    /// write the drawn sequence back into `a[..n]`. `n` is clamped to `a.len()`.
    pub fn shuffle_draw(&mut self, a: &mut [i32], n: usize, base: Option<i32>) {
        let n = n.min(a.len());
        let pool = &mut a[..n];
        // The original fills only when base >= 0; a negative base means "use contents".
        if let Some(base) = base.filter(|&b| b >= 0) {
            for (i, v) in pool.iter_mut().enumerate() {
                *v = base + i as i32;
            }
        }
        // `taken` replaces the original's -99999 marker; the draw order is identical.
        let mut taken = vec![false; n];
        let mut out = Vec::with_capacity(n);
        for remaining in (1..=n).rev() {
            let r = self.next() as usize % remaining;
            let idx = (0..n)
                .filter(|&i| !taken[i])
                .nth(r)
                .expect("r < remaining untaken elements");
            taken[idx] = true;
            out.push(pool[idx]);
        }
        pool.copy_from_slice(&out);
    }

    /// `ShuffleWithInverse` 0x410210: identity of length n, 1000 swaps, inverse, and the
    /// slot holding 0 (the correct answer). Consumes exactly 2000 values for n > 0.
    pub fn shuffle_with_inverse(&mut self, n: usize) -> (Vec<usize>, Vec<usize>, usize) {
        let mut perm: Vec<usize> = (0..n).collect();
        swap1000(self, &mut perm);
        let mut inv = vec![0; n];
        let mut correct = 0;
        for (i, &p) in perm.iter().enumerate() {
            inv[p] = i;
            if p == 0 {
                correct = i;
            }
        }
        (perm, inv, correct)
    }
}

fn swap1000<T>(rng: &mut Lcg, a: &mut [T]) {
    let n = a.len();
    if n == 0 {
        return;
    }
    for _ in 0..1000 {
        let r1 = rng.next() as usize % n;
        let r2 = rng.next() as usize % n;
        a.swap(r1, r2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advanced(seed: u32, calls: usize) -> Lcg {
        let mut l = Lcg::new(seed);
        for _ in 0..calls {
            l.next();
        }
        l
    }

    #[test]
    fn lcg_matches_msvc_rand() {
        let mut l = Lcg::new(1);
        let v: Vec<u32> = (0..5).map(|_| l.next()).collect();
        assert_eq!(v, [41, 18467, 6334, 26500, 19169]);
        let mut l = Lcg::new(0);
        let v: Vec<u32> = (0..3).map(|_| l.next()).collect();
        assert_eq!(v, [38, 7719, 21238]);
    }

    #[test]
    fn next_stays_in_range() {
        let mut l = Lcg::new(0xdead_beef);
        assert!((0..10_000).all(|_| l.next() <= 0x7fff));
    }

    #[test]
    fn shuffle_with_inverse_is_permutation() {
        for seed in 0..100u32 {
            let mut l = Lcg::new(seed.wrapping_mul(2_654_435_761));
            let start = l.state();
            let (perm, inv, correct) = l.shuffle_with_inverse(4);
            let mut sorted = perm.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, [0, 1, 2, 3], "seed {seed}");
            for i in 0..4 {
                assert_eq!(inv[perm[i]], i);
            }
            assert_eq!(perm[correct], 0);
            // Exactly 2000 draws: the state equals a fresh LCG advanced 2000 times.
            assert_eq!(l, advanced(start, 2000), "seed {seed}");
        }
    }

    /// Straight reading of F2.4's ShuffleSwap1000 text on a reference LCG.
    #[test]
    fn shuffle_swap1000_matches_reference() {
        let mut a = [1, 3, 7, 11, 15];
        let mut l = Lcg::new(777);
        l.shuffle_swap1000(&mut a);

        let mut b = [1, 3, 7, 11, 15];
        let mut r = Lcg::new(777);
        for _ in 0..1000 {
            let r1 = r.next() as usize % b.len();
            let r2 = r.next() as usize % b.len();
            b.swap(r1, r2);
        }
        assert_eq!(a, b);
        assert_eq!(l, r);
    }

    /// Reference from F2.4's text: take the r-th untaken element, r = next() % remaining.
    fn reference_draw(seed: u32, n: usize, base: i32) -> (Vec<i32>, Lcg) {
        let mut r = Lcg::new(seed);
        let mut pool: Vec<i32> = (0..n as i32).map(|i| base + i).collect();
        let mut out = Vec::new();
        while !pool.is_empty() {
            let k = r.next() as usize % pool.len();
            out.push(pool.remove(k));
        }
        (out, r)
    }

    #[test]
    fn shuffle_draw_matches_reference() {
        let mut a = [0i32; 5];
        let mut l = Lcg::new(12345);
        l.shuffle_draw(&mut a, 5, Some(1));
        let (expected, r) = reference_draw(12345, 5, 1);
        assert_eq!(a.to_vec(), expected);
        assert_eq!(l, r);
    }

    #[test]
    fn shuffle_draw_without_base_uses_contents_and_leaves_tail() {
        let mut a = [10, 20, 30, 99];
        let mut l = Lcg::new(5);
        l.shuffle_draw(&mut a, 3, None);
        let (expected, _) = reference_draw(5, 3, 0);
        let mapped: Vec<i32> = expected.iter().map(|&i| (i + 1) * 10).collect();
        assert_eq!(a[..3].to_vec(), mapped);
        assert_eq!(a[3], 99);
    }

    #[test]
    fn negative_base_is_treated_as_none() {
        let mut a = [10, 20, 30];
        let mut b = a;
        Lcg::new(9).shuffle_draw(&mut a, 3, Some(-1));
        Lcg::new(9).shuffle_draw(&mut b, 3, None);
        assert_eq!(a, b);
        let mut sorted = a;
        sorted.sort_unstable();
        assert_eq!(sorted, [10, 20, 30]);
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        let mut l = Lcg::new(1);
        l.shuffle_swap1000(&mut []);
        l.shuffle_draw(&mut [], 3, Some(0));
        assert_eq!(l.shuffle_with_inverse(0), (vec![], vec![], 0));
        assert_eq!(l, Lcg::new(1));
    }
}
