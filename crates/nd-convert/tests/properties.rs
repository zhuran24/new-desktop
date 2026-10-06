use nd_convert::{BackendKind, FrozenImage, FrozenInput, NativeItem, convert, decode, encode};
use proptest::prelude::*;
use serde_json::json;

fn input(backend: BackendKind, items: Vec<serde_json::Value>) -> FrozenInput {
    FrozenInput {
        backend,
        source_id: "property-source".into(),
        epoch: "epoch".into(),
        complete: true,
        images: Default::default(),
        items: items
            .into_iter()
            .enumerate()
            .map(|(i, payload)| NativeItem {
                position: i.to_string(),
                payload,
            })
            .collect(),
    }
}

proptest! {
    #[test]
    fn portable_neutral_messages_round_trip_through_each_codec(texts in prop::collection::vec(".{0,150}",1..20), bytes in prop::collection::vec(any::<u8>(),1..200)) {
        let image = FrozenImage::from_bytes("image/png",&bytes);
        let mut messages: Vec<_> = texts.iter().enumerate().map(|(i,s)|json!({"role":if i%2==0 {"user"} else {"assistant"},"content":[{"type":"text","text":s}]})).collect();
        messages.push(json!({"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":image.data}}]}));
        let original = decode(&input(BackendKind::Claude,messages)).unwrap();
        let portable: Vec<_> = original.entries.iter().map(|e|(&e.role,&e.parts)).collect();
        for backend in [BackendKind::Claude,BackendKind::Codex] {
            let encoded = encode(&original,backend).unwrap();
            let recovered = decode(&input(backend,encoded.items)).unwrap();
            let actual: Vec<_> = recovered.entries.iter().map(|e|(&e.role,&e.parts)).collect();
            prop_assert_eq!(&portable,&actual);
        }
    }

    #[test]
    fn incremental_batches_have_the_same_output_as_whole_conversion(texts in prop::collection::vec(".{0,100}",1..30), split in any::<usize>()) {
        let messages:Vec<_> = texts.iter().map(|s|json!({"role":"user","content":s})).collect();
        let all = input(BackendKind::Claude,messages);
        let mut prefix = all.clone();
        prefix.items.truncate(split%texts.len());
        let a = convert(&prefix,BackendKind::Codex,None).unwrap();
        let b = convert(&all,BackendKind::Codex,Some(&a.sync)).unwrap();
        let mut incremental = a.items;
        incremental.extend(b.items);
        prop_assert_eq!(incremental,convert(&all,BackendKind::Codex,None).unwrap().items);
        prop_assert!(convert(&all,BackendKind::Codex,Some(&b.sync)).unwrap().items.is_empty());
    }
}
