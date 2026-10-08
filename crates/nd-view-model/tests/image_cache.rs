use nd_view_model::ImageCache;

#[test]
fn repeated_snapshots_keep_recent_images_and_evict_the_oldest_use() {
    let mut cache = ImageCache::new(3);
    for id in ["a", "b", "z"] {
        assert!(cache.request(id));
        cache.complete(id, id.to_owned());
    }
    // 最近两张的散列恰好最小；它们每次快照都会被使用。
    assert!(!cache.request("a"));
    assert!(!cache.request("b"));
    assert!(cache.request("c"));
    cache.complete("c", "c".into());
    for _ in 0..10 {
        for id in ["a", "b", "c"] {
            assert!(!cache.request(id), "visible {id} must not download again");
            assert_eq!(cache.get(id).map(String::as_str), Some(id));
        }
    }
    cache.complete("z", "late".into());
    assert!(cache.get("z").is_none());
    assert!(cache.request("z"));
    assert!(cache.get("a").is_none());
}
