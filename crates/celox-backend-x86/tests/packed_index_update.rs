#![cfg(target_arch = "x86_64")]
#[path = "support/packed_index_fixture.rs"]
mod fixture;

#[test]
fn native_direct_updates_match_the_original_scan_and_preserve_other_bits() {
    let mut before = fixture::compile(false);
    let mut after = fixture::compile(true);
    for selector in 0u8..32 {
        for payload in [0u8, 1, 63, 255] {
            for fill in [0u8, 0xff, 0xa5] {
                let mut expected = [fill; 24];
                for bit in 0..6 {
                    let offset = selector as usize * 6 + bit;
                    let mask = 1u8 << (offset % 8);
                    expected[offset / 8] =
                        (expected[offset / 8] & !mask) | (((payload >> bit) & 1) << (offset % 8));
                }
                for compiled in [&mut before, &mut after] {
                    let bytes = compiled.bytes();
                    bytes[..80].fill(0xcd);
                    bytes[0] = selector;
                    bytes[8..32].fill(0xff); // high payload bits must not leak into neighbors
                    bytes[8] = payload;
                    bytes[32..56].fill(fill);
                    assert_eq!(compiled.execute(), 0);
                    assert_eq!(&compiled.bytes()[56..80], &expected);
                    assert_eq!(&compiled.bytes()[32..56], &[fill; 24]);
                }
            }
        }
    }
}
