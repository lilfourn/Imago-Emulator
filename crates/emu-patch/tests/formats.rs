//! Patches made by the tools hack authors use, Floating IPS and xdelta3,
//! applied to the made-up ROMs they were made from.
//! `scripts/make-patch-fixtures.sh` makes them again.

use std::path::Path;

use emu_patch::{Format, PatchError, apply, source_crc32};

const LIMIT: usize = 1 << 20;

/// Bytes that look random, as a ROM's code and data do, from `seed`.
fn noise(seed: u32, length: usize) -> Vec<u8> {
    let mut state = seed;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// A made-up 96 KiB ROM, with 4 KiB of padding in the middle.
fn source() -> Vec<u8> {
    let mut rom = noise(1, 96 * 1024);
    rom[0x9000..0xA000].fill(0xFF);
    rom
}

/// The source as a hack changes it: new bytes, a block moved, a pattern
/// repeated, bytes inserted, and the ROM grown with more padding.
fn target() -> Vec<u8> {
    let mut rom = source();
    rom[0x1000..0x1000 + 300].copy_from_slice(&noise(2, 300));
    rom.copy_within(0x8000..0x8800, 0x2000);
    for (at, byte) in rom[0x3000..0x3200].iter_mut().enumerate() {
        *byte = [0x12, 0x34, 0x56, 0x78][at % 4];
    }
    rom.splice(0x5000..0x5000, noise(3, 1024));
    rom.extend_from_slice(&noise(4, 4096));
    rom.resize(rom.len() + 8192, 0xFF);
    rom
}

/// A change IPS and linear BPS can make without writing the whole ROM,
/// as they cannot move bytes: new bytes, a run of one, and the ROM grown.
fn ips_target() -> Vec<u8> {
    let mut rom = source();
    rom[0x1000..0x1000 + 300].copy_from_slice(&noise(2, 300));
    rom[0x4000..0x4400].fill(0);
    rom.extend_from_slice(&noise(5, 256));
    rom
}

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Writes the made-up ROMs where `scripts/make-patch-fixtures.sh` asks.
#[test]
#[ignore = "run by scripts/make-patch-fixtures.sh"]
fn write_fixture_roms() {
    let folder = std::env::var("EMU_PATCH_FIXTURE_ROMS").expect("EMU_PATCH_FIXTURE_ROMS");
    let folder = Path::new(&folder);
    std::fs::write(folder.join("source.bin"), source()).expect("source");
    std::fs::write(folder.join("target.bin"), target()).expect("target");
    std::fs::write(folder.join("ips-target.bin"), ips_target()).expect("IPS target");
}

#[test]
fn applies_patches_from_floating_ips() {
    let ips = fixture("flips.ips");
    assert_eq!(Format::of(&ips), Some(Format::Ips));
    assert_eq!(apply(&ips, &source(), LIMIT), Ok(ips_target()));
    for (name, target) in [("flips.bps", target()), ("flips-linear.bps", ips_target())] {
        let bps = fixture(name);
        assert_eq!(Format::of(&bps), Some(Format::Bps));
        assert_eq!(
            source_crc32(&bps),
            Some(crc32fast::hash(&source())),
            "{name}"
        );
        assert_eq!(apply(&bps, &source(), LIMIT), Ok(target), "{name}");
    }
}

#[test]
fn applies_patches_from_xdelta3() {
    // Uncompressed and compressed with LZMA, xdelta3's default; each
    // in one window and in 16 KiB windows, whose LZMA streams run on
    // from one window to the next.
    let names = [
        "xdelta3.xdelta",
        "xdelta3-lzma.xdelta",
        "xdelta3-windows.xdelta",
        "xdelta3-lzma-windows.xdelta",
    ];
    for name in names {
        let patch = fixture(name);
        assert_eq!(Format::of(&patch), Some(Format::Vcdiff));
        assert_eq!(source_crc32(&patch), None);
        assert_eq!(apply(&patch, &source(), LIMIT), Ok(target()), "{name}");
    }
    assert_eq!(
        apply(&fixture("xdelta3-djw.xdelta"), &source(), LIMIT),
        Err(PatchError::Unsupported("xdelta's DJW compression"))
    );
}

#[test]
fn checked_formats_refuse_the_wrong_rom() {
    let other = noise(9, 96 * 1024);
    assert_eq!(
        apply(&fixture("flips.bps"), &other, LIMIT),
        Err(PatchError::WrongSource {
            expected: crc32fast::hash(&source()),
            actual: crc32fast::hash(&other),
        })
    );
    assert_eq!(
        apply(&fixture("flips.bps"), &target(), LIMIT),
        Err(PatchError::AlreadyPatched)
    );
    // xdelta checks only what it makes.
    assert_eq!(
        apply(&fixture("xdelta3.xdelta"), &other, LIMIT),
        Err(PatchError::WrongResult)
    );
}

#[test]
fn damaged_patches_fail_without_panicking() {
    let source = source();
    for name in [
        "flips.ips",
        "flips.bps",
        "xdelta3.xdelta",
        "xdelta3-lzma.xdelta",
    ] {
        let patch = fixture(name);
        // Cut short anywhere.
        for length in (0..patch.len()).step_by(7) {
            assert!(
                apply(&patch[..length], &source, LIMIT).is_err(),
                "{name} cut to {length}"
            );
        }
        // A byte changed anywhere.
        for at in (0..patch.len()).step_by(5) {
            let mut damaged = patch.clone();
            damaged[at] ^= 0x5A;
            let _ = apply(&damaged, &source, LIMIT);
        }
    }
}

#[test]
fn nothing_is_made_past_the_limit() {
    let small = 64 * 1024;
    for name in ["flips.ips", "flips.bps", "xdelta3.xdelta"] {
        assert_eq!(
            apply(&fixture(name), &source(), small),
            Err(PatchError::TooBig { limit: small }),
            "{name}"
        );
    }
}

/// A UPS patch, made here as no common tool makes them: the XOR of
/// `source` and `target`, in hunks.
fn ups(source: &[u8], target: &[u8]) -> Vec<u8> {
    fn number(out: &mut Vec<u8>, mut value: usize) {
        loop {
            let low = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(0x80 | low);
                return;
            }
            out.push(low);
            value -= 1;
        }
    }
    let byte = |rom: &[u8], at: usize| rom.get(at).copied().unwrap_or(0);
    let mut patch = b"UPS1".to_vec();
    number(&mut patch, source.len());
    number(&mut patch, target.len());
    let mut last = 0;
    let mut at = 0;
    while at < target.len() {
        if byte(source, at) == target[at] {
            at += 1;
            continue;
        }
        number(&mut patch, at - last);
        while at < target.len() && byte(source, at) != target[at] {
            patch.push(byte(source, at) ^ target[at]);
            at += 1;
        }
        patch.push(0);
        at += 1;
        last = at;
    }
    patch.extend_from_slice(&crc32fast::hash(source).to_le_bytes());
    patch.extend_from_slice(&crc32fast::hash(target).to_le_bytes());
    let crc = crc32fast::hash(&patch);
    patch.extend_from_slice(&crc.to_le_bytes());
    patch
}

#[test]
fn applies_ups_patches() {
    let patch = ups(&source(), &ips_target());
    assert_eq!(Format::of(&patch), Some(Format::Ups));
    assert_eq!(source_crc32(&patch), Some(crc32fast::hash(&source())));
    assert_eq!(apply(&patch, &source(), LIMIT), Ok(ips_target()));
    // Shrinking drops what lies past the target's end.
    let shrunk = &source()[..50_000];
    assert_eq!(
        apply(&ups(&source(), shrunk), &source(), LIMIT),
        Ok(shrunk.to_vec())
    );
    let mut damaged = patch.clone();
    damaged[10] ^= 1;
    assert_eq!(apply(&damaged, &source(), LIMIT), Err(PatchError::Damaged));
}
