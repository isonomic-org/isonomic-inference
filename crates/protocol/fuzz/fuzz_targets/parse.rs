// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Any bytes as a whole reply body for every format; whatever parses must
//! emit again as the next turn without a panic.
#![no_main]

use isonomic_model_protocol::{Chat, EmitOptions, Limits, Messages, Responses, WireClient};
use libfuzzer_sys::fuzz_target;

fn parse<W: WireClient>(data: &[u8]) {
    if let Ok((conversation, _)) = W::parse_body(data, &Limits::default()) {
        let opts = EmitOptions::default();
        let _ = Messages::emit(&conversation, &opts);
        let _ = Chat::emit(&conversation, &opts);
    }
}

fuzz_target!(|data: &[u8]| {
    parse::<Messages>(data);
    parse::<Chat>(data);
    parse::<Responses>(data);
});
