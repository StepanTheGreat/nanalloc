use std::process::Command;


#[cfg_attr(miri, ignore = "Not testable by miri")]
#[test]
fn test_wasm() {
    let out = Command::new("sh")
        .arg("./tests/wasm_tests.sh")
        .spawn()
        .unwrap()
        .wait_with_output()
        .unwrap();

    assert!(out.status.success());
}