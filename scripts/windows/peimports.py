"""The DLLs a Windows executable or DLL imports (its import and
delay-load tables), read from its PE headers."""

import struct


def imports(path):
    with open(path, "rb") as f:
        data = f.read()
    (pe,) = struct.unpack_from("<I", data, 0x3C)
    if data[pe : pe + 4] != b"PE\0\0":
        return []
    coff = pe + 4
    sections, opt_size = struct.unpack_from("<H12xH", data, coff + 2)
    opt = coff + 20
    magic = struct.unpack_from("<H", data, opt)[0]
    dirs = opt + (112 if magic == 0x20B else 96)
    table = opt + opt_size
    secs = []
    for i in range(sections):
        s = table + 40 * i
        vsize, vaddr, rsize, raddr = struct.unpack_from("<IIII", data, s + 8)
        secs.append((vaddr, max(vsize, rsize), raddr))

    def off(rva):
        for vaddr, size, raddr in secs:
            if vaddr <= rva < vaddr + size:
                return rva - vaddr + raddr
        return None

    def name(rva):
        o = off(rva)
        if o is None:
            return None
        end = data.index(b"\0", o)
        return data[o:end].decode("ascii", "replace")

    out = []
    # Imports (descriptor: 20 bytes, name at +12) and delay-loads (32 bytes,
    # name at +4).
    for index, size, at in ((1, 20, 12), (13, 32, 4)):
        rva = struct.unpack_from("<I", data, dirs + 8 * index)[0]
        o = off(rva) if rva else None
        while o is not None and o + size <= len(data):
            n = struct.unpack_from("<I", data, o + at)[0]
            if n == 0:
                break
            dll = name(n)
            if dll:
                out.append(dll)
            o += size
    return out


if __name__ == "__main__":
    import sys

    for dll in imports(sys.argv[1]):
        print(dll)
