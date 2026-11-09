#!/usr/bin/env python3
"""Generate a minimal PDF with a *direct* (inline) highlight annotation.

No third-party dependency.  This is useful for testing readers/writers that
must handle inline annotation dictionaries instead of indirect objects.

Usage:
    python3 scripts/make_inline_sample.py [output.pdf]
"""
from __future__ import annotations

import sys
from pathlib import Path


def build() -> bytes:
    content = b"BT /F1 24 Tf 50 700 Td (Inline annotation sample) Tj ET\n"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R "
        b"/Annots [<< /Type /Annot /Subtype /Highlight "
        b"/Rect [50 100 450 118] "
        b"/QuadPoints [50 118 450 118 50 100 450 100] "
        b"/C [1 1 0] /CA 0.35 /F 4 /NM (inline-sample) "
        b"/Contents (inline highlight) >>] >>",
        b"<< /Length %d >>\nstream\n" % len(content) + content + b"endstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for number, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += f"{number} 0 obj\n".encode()
        out += body
        out += b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objects) + 1}\n".encode()
    out += b"0000000000 65535 f \n"
    for offset in offsets:
        out += f"{offset:010d} 00000 n \n".encode()
    out += (
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{xref}\n%%EOF\n"
    ).encode()
    return bytes(out)


def main() -> int:
    target = Path(sys.argv[1] if len(sys.argv) > 1 else "inline_annotation_sample.pdf")
    target.write_bytes(build())
    print(f"wrote inline annotation sample: {target} ({target.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
