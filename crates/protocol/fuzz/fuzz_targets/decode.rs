//! Any bytes, split anywhere, into every stream decoder and error decoder:
//! errors are fine, panics are not.
#![no_main]

use isonomic_model_protocol::{Chat, Limits, Messages, Responses, StreamDecoder, WireClient};
use libfuzzer_sys::fuzz_target;

fn feed<W: WireClient>(data: &[u8], step: usize) {
    let mut decoder = StreamDecoder::<W>::new(Limits::default());
    for piece in data.chunks(step) {
        if decoder.push(piece).is_err() {
            return;
        }
    }
    let _ = decoder.finish();
}

fuzz_target!(|data: &[u8]| {
    let step = 1 + usize::from(data.first().copied().unwrap_or(0) % 64);
    feed::<Messages>(data, step);
    feed::<Chat>(data, step);
    feed::<Responses>(data, step);
    let _ = Messages::decode_error(Some(500), data);
    let _ = Chat::decode_error(Some(500), data);
    let _ = Responses::decode_error(Some(500), data);
});
