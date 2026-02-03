use bytes::Bytes;
use futures_util::StreamExt;
use litellm_rs::stream::{parse_anthropic_sse_stream, parse_sse_stream};

#[tokio::test]
async fn parse_sse_basic() {
    let data = Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
    let stream = futures_util::stream::iter(vec![Ok(data)]);
    let mut parsed = parse_sse_stream(stream);
    let chunk = parsed.next().await.unwrap().unwrap();
    assert_eq!(chunk.content, "hi");
}

#[tokio::test]
async fn parse_anthropic_sse_basic() {
    let payload = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
    );
    let stream = futures_util::stream::iter(vec![Ok(Bytes::from(payload))]);
    let mut parsed = parse_anthropic_sse_stream(stream);
    let chunk = parsed.next().await.unwrap().unwrap();
    assert_eq!(chunk.content, "hello");
}
