use std::fs;

#[test]
fn fixtures_present() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/manifests");
    let bin = fs::read(format!("{dir}/oa4_45.bin")).unwrap();
    assert_eq!(bin.len(), 11433);
    let golden = fs::read_to_string(format!("{dir}/golden/oa4_45.bin.golden.txt")).unwrap();
    assert_eq!(golden.lines().count(), 45);
}
