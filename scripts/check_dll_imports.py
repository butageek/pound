#!/usr/bin/env python3
"""Fail if pound.exe imports any DLL outside the Windows system set.

Prevents shipping releases that die at startup with STATUS_DLL_NOT_FOUND
(0xC0000135) on machines lacking a redistributable — e.g. WebView2Loader.dll
(from mingw builds) or VCRUNTIME140.dll (MSVC builds without crt-static).

Usage: check_dll_imports.py <path-to-exe>
"""
import struct
import sys

SYSTEM_DLLS = {
    "advapi32.dll", "bcrypt.dll", "bcryptprimitives.dll", "combase.dll",
    "comctl32.dll", "crypt32.dll", "dwmapi.dll", "gdi32.dll", "imm32.dll",
    "kernel32.dll", "msvcrt.dll", "ncrypt.dll", "ntdll.dll", "ole32.dll",
    "oleaut32.dll", "secur32.dll", "shell32.dll", "shlwapi.dll",
    "ucrtbase.dll", "user32.dll", "userenv.dll", "ws2_32.dll",
}


def is_system(name: str) -> bool:
    lower = name.lower()
    return lower in SYSTEM_DLLS or lower.startswith("api-ms-")


def import_table(path: str) -> set:
    data = open(path, "rb").read()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    import_rva, _size = struct.unpack_from("<II", data, opt + (120 if magic == 0x20B else 104))
    section_count = struct.unpack_from("<H", data, pe + 6)[0]
    section_off = opt + struct.unpack_from("<H", data, pe + 20)[0]
    sections = []
    for i in range(section_count):
        s = section_off + i * 40
        vsize, vaddr, _rsize, roff = struct.unpack_from("<IIII", data, s + 8)
        sections.append((vaddr, vsize, roff))

    def rva_to_off(rva):
        for vaddr, vsize, roff in sections:
            if vaddr <= rva < vaddr + max(vsize, 0x1000):
                return roff + (rva - vaddr)
        return None

    off = rva_to_off(import_rva)
    names = set()
    if off is None:
        return names
    while True:
        entry = data[off : off + 20]
        if entry == b"\x00" * 20:
            break
        name_rva = struct.unpack_from("<I", entry, 12)[0]
        name_off = rva_to_off(name_rva)
        names.add(data[name_off : data.index(b"\x00", name_off)].decode())
        off += 20
    return names


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    foreign = sorted(d for d in import_table(sys.argv[1]) if not is_system(d))
    if foreign:
        print("FAIL: pound.exe imports non-system DLLs: " + ", ".join(foreign))
        print("      Ship them next to the exe or link them statically instead.")
        sys.exit(1)
    print("OK: pound.exe imports only Windows system DLLs")


if __name__ == "__main__":
    main()
