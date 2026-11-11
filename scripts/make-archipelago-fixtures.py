"""Makes the Archipelago patches in crates/emu-patch/tests/fixtures with
Archipelago's own code, from the made-up ROMs the tests make.

    uv run --with bsdiff4 python3 scripts/make-archipelago-fixtures.py FILES_PY ROMS OUT

FILES_PY is Archipelago's `worlds/Files.py`, which writes every patch and
applies it; scripts/make-patch-fixtures.sh fetches it. ROMS holds
`source.bin` and `target.bin`. Two patches are written into OUT:

- archipelago.apemerald: as Pokémon Emerald's and Red and Blue's are, a
  bsdiff of source to target, then edits ("tokens") of every kind. Its
  edits are the ones `archipelago_target` makes in tests/formats.rs.
- archipelago-delta.apred: as patches were before they had procedures, a
  manifest that names none and `delta.bsdiff4`.

Each is applied again by Archipelago's code, and the CRC-32 of what that
makes is printed, for the tests to hold this crate's result to.
"""

import hashlib
import importlib.util
import json
import os
import sys
import tempfile
import zipfile
import zlib

import bsdiff4

files_py, roms, out = sys.argv[1:4]
spec = importlib.util.spec_from_file_location("Files", files_py)
Files = importlib.util.module_from_spec(spec)
sys.modules["Files"] = Files
spec.loader.exec_module(Files)

source = open(os.path.join(roms, "source.bin"), "rb").read()
target = open(os.path.join(roms, "target.bin"), "rb").read()
GAME = "Imago Test"


class Patch(Files.APProcedurePatch, Files.APTokenMixin):
    game = GAME
    hash = hashlib.md5(source).hexdigest()
    patch_file_ending = ".apemerald"
    result_file_ending = ".gba"
    procedure = [
        ("apply_bsdiff4", ["base_patch.bsdiff4"]),
        ("apply_tokens", ["token_data.bin"]),
    ]

    @classmethod
    def get_source_data(cls) -> bytes:
        return source


class Extension(Files.APPatchExtension):
    game = GAME


def applied(path: str) -> bytes:
    """What Archipelago makes of the patch at `path`."""
    with tempfile.TemporaryDirectory() as folder:
        made = os.path.join(folder, "made.gba")
        Patch(path).patch(made)
        return open(made, "rb").read()


kinds = Files.APTokenTypes
path = os.path.join(out, "archipelago.apemerald")
patch = Patch(path, player=1, player_name="Player")
patch.write_file("base_patch.bsdiff4", bsdiff4.diff(source, target))
patch.write_token(kinds.WRITE, 0x10, b"IMAGO")
patch.write_token(kinds.COPY, 0x200, (0x40, 0x4000))
patch.write_token(kinds.RLE, 0x300, (0x20, 0x5A))
patch.write_token(kinds.AND_8, 0x400, 0x0F)
patch.write_token(kinds.OR_8, 0x401, 0x80)
patch.write_token(kinds.XOR_8, 0x402, 0xFF)
patch.write_token(kinds.WRITE, len(target), b"END")
patch.write_file("token_data.bin", patch.get_token_binary())
patch.write()
print(f"archipelago.apemerald: crc32 {zlib.crc32(applied(path)):08x}")

# The same bsdiff alone, under a manifest from before procedures.
delta = os.path.join(out, "archipelago-delta.apred")
manifest = {
    "server": "",
    "player": 1,
    "player_name": "Player",
    "game": GAME,
    "base_checksum": Patch.hash,
    "result_file_ending": ".gb",
    "patch_file_ending": ".apred",
    "compatible_version": 4,
    "version": 4,
}
with zipfile.ZipFile(delta, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as zip:
    zip.writestr("archipelago.json", json.dumps(manifest))
    zip.writestr("delta.bsdiff4", bsdiff4.diff(source, target), compress_type=zipfile.ZIP_STORED)
made = applied(delta)
assert made == target
print(f"archipelago-delta.apred: crc32 {zlib.crc32(made):08x}")
