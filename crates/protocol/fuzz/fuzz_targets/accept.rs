//! Any bytes as a request body for every format; whatever is accepted must
//! emit again without a panic.
#![no_main]

use isonomic_model_protocol::{Chat, EmitOptions, Limits, Messages, Responses, WireClient, WireServer};
use libfuzzer_sys::fuzz_target;

fn accept<W: WireServer>(data: &[u8]) {
    if let Ok(accepted) = W::accept_body(data, &Limits::default()) {
        let opts = EmitOptions {
            blobs: &accepted.blobs,
            ..EmitOptions::default()
        };
        let _ = Messages::emit(&accepted.conversation, &opts);
        let _ = Chat::emit(&accepted.conversation, &opts);
        let _ = Responses::emit(&accepted.conversation, &opts);
    }
}

fuzz_target!(|data: &[u8]| {
    accept::<Messages>(data);
    accept::<Chat>(data);
    accept::<Responses>(data);
});
