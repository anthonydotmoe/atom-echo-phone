use heapless::Vec;

#[derive(Debug, Clone)]
pub struct JitterFrame<const FRAME: usize> {
    pub seq: u16,
    pub samples: Vec<i16, FRAME>,
}

#[derive(Debug)]
pub struct JitterBuffer<const CAP: usize, const FRAME: usize> {
    next_seq: Option<u16>,
    frames: Vec<JitterFrame<FRAME>, CAP>,
}

impl<const CAP: usize, const FRAME: usize> JitterBuffer<CAP, FRAME> {
    pub fn new() -> Self {
        Self {
            next_seq: None,
            frames: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.next_seq = None;
        self.frames.clear();
    }

    pub fn push_frame(&mut self, seq: u16, samples: &[i16]) {
        if self.frames.is_full() {
            let _ = self.frames.remove(0);
        }

        if self.frames.iter().any(|f| f.seq == seq) {
            return;
        }

        let mut buf: Vec<i16, FRAME> = Vec::new();
        for s in samples.iter().copied().take(FRAME) {
            let _ = buf.push(s);
        }
        while buf.len() < FRAME {
            let _ = buf.push(0);
        }

        let _ = self.frames.push(JitterFrame { seq, samples: buf });
    }

    pub fn pop_frame(&mut self) -> (Vec<i16, FRAME>, bool) {
        if self.next_seq.is_none() {
            if let Some(min_seq) = self.frames.iter().map(|f| f.seq).min() {
                self.next_seq = Some(min_seq);
            }
        }

        let expected = match self.next_seq {
            Some(s) => s,
            None => return (silence_frame::<FRAME>(), false),
        };

        if let Some(pos) = self.frames.iter().position(|f| f.seq == expected) {
            let frame = self.frames.remove(pos);
            self.next_seq = Some(expected.wrapping_add(1));
            return (frame.samples, true);
        }

        if self.frames.is_full() {
            if let Some(pos) = self
                .frames
                .iter()
                .enumerate()
                .min_by_key(|(_, f)| f.seq)
                .map(|(pos, _)| pos)
            {
                if let Some(frame) = self.frames.get(pos).cloned() {
                    let _ = self.frames.remove(pos);
                    self.next_seq = Some(frame.seq.wrapping_add(1));
                    return (frame.samples, true);
                }
            }
        }

        self.next_seq = Some(expected.wrapping_add(1));
        (silence_frame::<FRAME>(), false)
    }
}

fn silence_frame<const FRAME: usize>() -> Vec<i16, FRAME> {
    let mut buf: Vec<i16, FRAME> = Vec::new();
    for _ in 0..FRAME {
        let _ = buf.push(0);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_buffer_reordering() {
        let mut jb: JitterBuffer<4, 4> = JitterBuffer::new();
        jb.push_frame(2, &[20, 21, 22, 23]);
        jb.push_frame(1, &[10, 11, 12, 13]);

        let (f1, ok1) = jb.pop_frame();
        assert!(ok1);
        assert_eq!(f1[..], [10, 11, 12, 13]);

        let (f2, ok2) = jb.pop_frame();
        assert!(ok2);
        assert_eq!(f2[..], [20, 21, 22, 23]);

        let (f3, ok3) = jb.pop_frame();
        assert!(!ok3);
        assert_eq!(f3, silence_frame::<4>());
    }

    #[test]
    fn jitter_buffer_drops_and_underflow() {
        let mut jb: JitterBuffer<3, 3> = JitterBuffer::new();
        jb.push_frame(5, &[1, 2, 3]);

        let (f1, ok1) = jb.pop_frame();
        assert!(ok1);
        assert_eq!(f1[..], [1, 2, 3]);

        let (f2, ok2) = jb.pop_frame();
        assert!(!ok2);
        assert_eq!(f2, silence_frame::<3>());

        let (f3, ok3) = jb.pop_frame();
        assert!(!ok3);
        assert_eq!(f3, silence_frame::<3>());
    }
}
