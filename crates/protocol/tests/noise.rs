//! Decoders never panic on broken input: random bytes, and the fixture
//! streams with bytes flipped, dropped and repeated. Errors are fine.
//!
//! A deterministic generator keeps this hermetic; long fuzzing runs come
//! with the fuzz targets.

use std::path::Path;

use isonomic_model_protocol::{Chat, Limits, Messages, StreamDecoder, Wire};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn feed<W: Wire>(bytes: &[u8], step: usize) {
    let mut decoder = StreamDecoder::<W>::new(Limits::default());
    for piece in bytes.chunks(step.max(1)) {
        if decoder.push(piece).is_err() {
            return;
        }
    }
    let _ = decoder.finish();
}

fn seeds(format: &str) -> Vec<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format);
    let mut out: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sse"))
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    out.sort();
    out
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut bytes = seed.to_vec();
    for _ in 0..1 + rng.below(8) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        match rng.below(4) {
            0 => bytes[at] = rng.next() as u8,
            1 => {
                bytes.remove(at);
            }
            2 => {
                let end = (at + rng.below(32)).min(bytes.len());
                let copy = bytes[at..end].to_vec();
                bytes.splice(at..at, copy);
            }
            _ => bytes.insert(at, b"{}[]\":,\n0-e\\"[rng.below(12)]),
        }
    }
    bytes
}

fn shake<W: Wire>(format: &str, rounds: usize) {
    let seeds = seeds(format);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..rounds {
        let seed = &seeds[rng.below(seeds.len())];
        let input = mutate(&mut rng, seed);
        feed::<W>(&input, 1 + rng.below(64));
        let noise: Vec<u8> = (0..rng.below(512)).map(|_| rng.next() as u8).collect();
        feed::<W>(&noise, 1 + rng.below(64));
        let _ = W::decode_response_body(&input, &Limits::default());
        let _ = W::decode_error(Some(500), &input);
    }
}

#[test]
fn the_messages_decoder_survives_noise() {
    shake::<Messages>("messages", 4000);
}

#[test]
fn the_chat_decoder_survives_noise() {
    shake::<Chat>("chat", 4000);
}
