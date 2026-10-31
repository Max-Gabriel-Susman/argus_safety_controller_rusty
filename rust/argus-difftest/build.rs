//! Compiles csrc/shim.c, and with it the firmware's argus_replay_client.h
//! and argus_wire.h straight from safety_controller/src: the tests run the
//! C exactly as the firmware builds it, not a copy that could drift.

fn main() {
    let c_src = "../../safety_controller/src";
    println!("cargo::rerun-if-changed=csrc/shim.c");
    println!("cargo::rerun-if-changed={c_src}/argus_replay_client.h");
    println!("cargo::rerun-if-changed={c_src}/argus_wire.h");

    cc::Build::new()
        .file("csrc/shim.c")
        .include(c_src)
        .std("c11")
        .warnings(true)
        .extra_warnings(true)
        .compile("argus_difftest_shim");
}
