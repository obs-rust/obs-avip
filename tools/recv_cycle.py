#!/usr/bin/env -S uv run --script
"""Create, use and destroy NDI receivers in one process, in a loop.

Shows whether a sender keeps serving a process after many connections
(docs/status.md, "Known issue outside this plugin"). No OBS needed, only the
NDI runtime.

    tools/recv_cycle.py <libndi path> <iterations> <active s> <gap s> <source name part> [...]

Each iteration, for each source in turn (like an OBS scene switch): create a
receiver (color format UYVY_BGRA and bandwidth highest, as obs-avip and
DistroAV use with latency "Normal"), capture video for <active s>, destroy it,
wait <gap s>. One line per receiver: iteration, source, frames, time to the
first frame, process memory (Linux: RSS; macOS: peak RSS). The loop stops
after 3 receivers in a row got no frame.

Options (environment):
    RECV_NAME=<name>      receiver name (default "recv-cycle")
    UNIQUE=1              append the iteration to the receiver name
    BW=0                  bandwidth lowest instead of highest
    REUSE=1               one receiver per source, re-connected with recv_connect
    WAIT_AFTER_STALL=<s>  after a stall, wait <s> s and try 3 more receivers

Example (Linux):
    tools/recv_cycle.py /usr/local/lib/libndi.so.6 25 2 0.5 "CAMERA (Stream1)"
"""

import ctypes as C
import os
import sys
import time

if len(sys.argv) < 6:
    sys.exit(__doc__)
lib, iters, active, gap = sys.argv[1], int(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
wanted = sys.argv[5:]
ndi = C.CDLL(lib)

RECV_NAME = os.environ.get("RECV_NAME", "recv-cycle")
UNIQUE = bool(os.environ.get("UNIQUE"))
BANDWIDTH = 0 if os.environ.get("BW") == "0" else 100  # lowest / highest
REUSE = bool(os.environ.get("REUSE"))
WAIT_AFTER_STALL = float(os.environ.get("WAIT_AFTER_STALL", "0"))
UYVY_BGRA, VIDEO = 1, 1


class Source(C.Structure):
    _fields_ = [("name", C.c_char_p), ("url", C.c_char_p)]


class FindCreate(C.Structure):
    _fields_ = [("show_local", C.c_bool), ("groups", C.c_char_p), ("extra_ips", C.c_char_p)]


class RecvCreate(C.Structure):
    _fields_ = [("source", Source), ("color_format", C.c_int), ("bandwidth", C.c_int),
                ("allow_fields", C.c_bool), ("recv_name", C.c_char_p)]


ndi.NDIlib_find_create_v2.restype = C.c_void_p
ndi.NDIlib_find_create_v2.argtypes = [C.POINTER(FindCreate)]
ndi.NDIlib_find_wait_for_sources.argtypes = [C.c_void_p, C.c_uint32]
ndi.NDIlib_find_get_current_sources.restype = C.POINTER(Source)
ndi.NDIlib_find_get_current_sources.argtypes = [C.c_void_p, C.POINTER(C.c_uint32)]
ndi.NDIlib_find_destroy.argtypes = [C.c_void_p]
ndi.NDIlib_recv_create_v3.restype = C.c_void_p
ndi.NDIlib_recv_create_v3.argtypes = [C.POINTER(RecvCreate)]
ndi.NDIlib_recv_connect.argtypes = [C.c_void_p, C.c_void_p]
ndi.NDIlib_recv_capture_v3.argtypes = [C.c_void_p, C.c_void_p, C.c_void_p, C.c_void_p, C.c_uint32]
ndi.NDIlib_recv_free_video_v2.argtypes = [C.c_void_p, C.c_void_p]
ndi.NDIlib_recv_destroy.argtypes = [C.c_void_p]
ndi.NDIlib_version.restype = C.c_char_p
if not ndi.NDIlib_initialize():
    sys.exit("NDIlib_initialize failed")
print("NDI", ndi.NDIlib_version().decode(), flush=True)


def memory_mb():
    try:
        for line in open("/proc/self/status"):
            if line.startswith("VmRSS"):
                return int(line.split()[1]) // 1024
    except OSError:
        import resource  # macOS: peak RSS in bytes

        return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss // 2**20
    return -1


def find(names, wait_s=10):
    """{name part: (name, url)}, copied before the finder is destroyed."""
    fd = ndi.NDIlib_find_create_v2(C.byref(FindCreate(True, None, None)))
    found, t_end = {}, time.time() + wait_s
    while time.time() < t_end and len(found) < len(names):
        ndi.NDIlib_find_wait_for_sources(fd, 1000)
        n = C.c_uint32(0)
        arr = ndi.NDIlib_find_get_current_sources(fd, C.byref(n))
        for i in range(n.value):
            for w in names:
                if w in arr[i].name.decode():
                    found[w] = (arr[i].name, arr[i].url)
    ndi.NDIlib_find_destroy(fd)
    return found


srcs = find(wanted)
missing = [w for w in wanted if w not in srcs]
if missing:
    sys.exit(f"not found: {missing}")
for nm, url in srcs.values():
    print("source", nm.decode(), (url or b"").decode(), flush=True)

frame = (C.c_uint8 * 512)()  # larger than NDIlib_video_frame_v2_t
kept = {}
zero_streak, waited, it = 0, False, 0
while it < iters:
    it += 1
    for w in wanted:
        nm, url = srcs[w]
        name = f"{RECV_NAME}-{it}" if UNIQUE else RECV_NAME
        rc = RecvCreate(Source(nm, url), UYVY_BGRA, BANDWIDTH, False, name.encode())
        t0 = time.time()
        if REUSE:
            if w not in kept:
                kept[w] = ndi.NDIlib_recv_create_v3(C.byref(rc))
            r = kept[w]
            source = Source(nm, url)
            ndi.NDIlib_recv_connect(r, C.byref(source))
        else:
            r = ndi.NDIlib_recv_create_v3(C.byref(rc))
        frames, first = 0, None
        while time.time() - t0 < active:
            if ndi.NDIlib_recv_capture_v3(r, frame, None, None, 100) == VIDEO:
                frames += 1
                if first is None:
                    first = time.time() - t0
                ndi.NDIlib_recv_free_video_v2(r, frame)
        if REUSE:
            ndi.NDIlib_recv_connect(r, None)
        else:
            ndi.NDIlib_recv_destroy(r)
        ff = f"{first * 1000:.0f} ms" if first is not None else "none"
        print(f"{time.strftime('%H:%M:%S')} it={it} src={w} frames={frames} first={ff} "
              f"mem={memory_mb()} MB", flush=True)
        zero_streak = zero_streak + 1 if frames == 0 else 0
        time.sleep(gap)
    if zero_streak >= 3:
        print("STALL: 3 receivers in a row got no frame", flush=True)
        if WAIT_AFTER_STALL and not waited:
            waited, zero_streak = True, 0
            print(f"waiting {WAIT_AFTER_STALL:.0f} s, then trying again", flush=True)
            time.sleep(WAIT_AFTER_STALL)
            iters = it + 3
            continue
        break

for r in kept.values():
    ndi.NDIlib_recv_destroy(r)
print("a new finder sees:", list(find(wanted, 5)), flush=True)
