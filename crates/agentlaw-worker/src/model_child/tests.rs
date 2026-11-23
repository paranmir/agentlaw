use super::*;

#[test]
fn every_admitted_job_survives_json_framing_including_escape_expansion() {
    // The broker admits 4 MiB of UTF-8, not 4 MiB of JSON. This is a transport
    // round trip, not an embedding-quality dataset or a copied encoder.
    for character in ['a', '\0'] {
        let text = character.to_string().repeat(4 * 1024 * 1024);
        let bytes = encode(&Input::Embed {
            overflow: crate::model_child::Overflow::Reject,
            id: 1,
            attempt: 1,
            text: text.clone(),
        })
        .expect("broker-admitted input must fit the private transport");
        let Input::Embed { text: actual, .. } = read(&mut &bytes[..]).unwrap() else {
            panic!("input changed its meaning in transit");
        };
        assert_eq!(actual, text);
    }
}

#[test]
fn truncated_and_oversized_frames_are_errors_not_empty_messages() {
    assert!(read::<Input>(&mut &[0, 0, 0][..]).is_err());
    assert!(read::<Input>(&mut &[0, 0, 0, 9, b'{'][..]).is_err());
    // Header alone must reject the allocation, without reading/allocating the body.
    assert!(read::<Input>(&mut &u32::MAX.to_be_bytes()[..]).is_err());
}
