//! Decoding a long stream holds constant memory: only the current line and
//! event are buffered, and decoders keep no reply text.

use isonomic_model_protocol::{Chat, Delta, Limits, Messages, StreamDecoder, WireClient};

/// Feed `events` copies of `event` in 4 KiB pieces; returns the largest
/// buffer the decoder held and the text it decoded.
fn run<W: WireClient>(head: &str, event: &str, tail: &str, total: usize) -> (usize, usize) {
    let mut stream = String::from(head);
    while stream.len() < total {
        stream.push_str(event);
    }
    stream.push_str(tail);
    let mut decoder = StreamDecoder::<W>::new(Limits::default());
    let (mut peak, mut text) = (0, 0);
    for piece in stream.as_bytes().chunks(4096) {
        for delta in decoder.push(piece).unwrap() {
            if let Delta::Text { text: t } = delta {
                text += t.len();
            }
        }
        peak = peak.max(decoder.buffered_bytes());
    }
    decoder.finish().unwrap();
    (peak, text)
}

const MB: usize = 1 << 20;

#[test]
fn a_one_megabyte_chat_stream_decodes_in_constant_memory() {
    let event = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"abcdefgh\"}}]}\n\n";
    let (peak_1, text_1) = run::<Chat>("", event, "data: [DONE]\n\n", MB);
    let (peak_4, text_4) = run::<Chat>("", event, "data: [DONE]\n\n", 4 * MB);
    assert!(text_1 > 100_000 && text_4 > 4 * 100_000);
    // The buffer never holds more than one event, whatever the length.
    assert!(peak_1 <= event.len(), "peak {peak_1}");
    assert_eq!(peak_1, peak_4);
}

#[test]
fn a_one_megabyte_messages_stream_decodes_in_constant_memory() {
    let head = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\"}}\n\n\
        data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n";
    let event = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"abcdefgh\"}}\n\n";
    let tail = "data: {\"type\":\"content_block_stop\",\"index\":0}\n\ndata: {\"type\":\"message_stop\"}\n\n";
    let (peak_1, text_1) = run::<Messages>(head, event, tail, MB);
    let (peak_4, _) = run::<Messages>(head, event, tail, 4 * MB);
    assert!(text_1 > 50_000);
    assert!(peak_1 <= event.len(), "peak {peak_1}");
    assert_eq!(peak_1, peak_4);
}
