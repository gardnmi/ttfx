use ttfx::utils::ordered_map::OrderedMap;

#[test]
fn handles_follow_names_through_relocation_replacement_and_clone() {
    let mut map = OrderedMap::new();
    map.insert("before", 1);
    map.insert("active", 2);
    let handle = map.handle("active").unwrap();
    for index in 0..128 {
        map.insert(index.to_string(), index);
    }
    assert_eq!(map.get_handle(&handle), Some(&2));
    map.remove("before");
    assert_eq!(map.handle_slot(&handle), Some(0));
    map.insert("active", 999);
    assert_eq!(map.get_handle(&handle), Some(&999));
    let mut cloned = map.clone();
    cloned.remove("active");
    cloned.insert("active", 3);
    assert_eq!(cloned.get_handle(&handle), Some(&3));
    assert_eq!(map.get_handle(&handle), Some(&999));
    map.clear();
    map.insert("replacement", 4);
    assert_eq!(map.get_handle(&handle), None);
    map.insert("active", 5);
    assert_eq!(map.get_handle(&handle), Some(&5));
    assert_eq!(cloned.get_handle(&handle), Some(&3));
}
