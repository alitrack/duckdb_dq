#!/usr/bin/env python3
"""Build semantic.duckdb_extension: cargo build + append metadata footer."""
import shutil, sys, os

SO = "target/release/libsemantic.so"
OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/semantic.duckdb_extension"

os.system("cargo build --release")

def make_field(s):
    b = s.encode('ascii')
    f = bytearray(32)
    f[:len(b)] = b
    return bytes(f)

shutil.copy(SO, OUT)
footer = bytearray()
for s in ["", "", "", "C_STRUCT", "v0.1.0", "v1.2.0", "linux_amd64", "4"]:
    footer.extend(make_field(s))
footer.extend(b'\x00' * 256)

with open(OUT, 'ab') as f:
    f.write(footer)
size = os.path.getsize(OUT)
print(f"✅ {OUT} ({size} bytes)")
