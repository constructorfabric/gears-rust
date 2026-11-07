#!/usr/bin/env python3
"""Generic disk I/O stress with content verification. Stdlib only, no project code.

Writes batches of small deterministic files, reads them back, verifies sha256,
churns older batches, and records every OSError without ever aborting.
"""
import argparse
import hashlib
import json
import os
import random
import shutil
import threading
import time

GIB = 1024 ** 3
MAX_DISTINCT_ERRORS = 50


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.files_written = 0
        self.bytes_written = 0
        self.files_verified = 0
        self.verify_mismatches = 0
        self.os_errors = 0
        self.errors = []
        self._seen = set()
        self.mismatch_samples = []

    def add(self, **kw):
        with self.lock:
            for k, v in kw.items():
                setattr(self, k, getattr(self, k) + v)

    def error(self, exc, path, phase):
        with self.lock:
            self.os_errors += 1
            key = (exc.errno, phase, type(exc).__name__)
            if key in self._seen or len(self.errors) >= MAX_DISTINCT_ERRORS:
                return
            self._seen.add(key)
            self.errors.append({
                "errno": exc.errno,
                "name": type(exc).__name__,
                "msg": str(exc),
                "path": str(path),
                "phase": phase,
                "t": int(time.time()),
            })

    def mismatch(self, path):
        with self.lock:
            self.verify_mismatches += 1
            if len(self.mismatch_samples) < MAX_DISTINCT_ERRORS:
                self.mismatch_samples.append(str(path))


def payload(seed, size):
    rng = random.Random(seed)
    return rng.getrandbits(size * 8).to_bytes(size, "little")


def file_plan(seed, idx):
    rng = random.Random("%s-%d" % (seed, idx))
    size = rng.randint(4, 64) * 1024
    return size, rng.randrange(10) == 0


def write_batch(root, tid, batch, nfiles, stats):
    seed = "t%d-b%d" % (tid, batch)
    bdir = os.path.join(root, "t%d" % tid, "s%02d" % (batch % 32), "b%d" % batch)
    entries = []
    try:
        os.makedirs(bdir, exist_ok=True)
    except OSError as e:
        stats.error(e, bdir, "mkdir")
        return None
    for i in range(nfiles):
        size, do_fsync = file_plan(seed, i)
        data = payload("%s-%d" % (seed, i), size)
        path = os.path.join(bdir, "f%d.bin" % i)
        try:
            with open(path, "wb") as f:
                f.write(data)
                if do_fsync:
                    f.flush()
                    os.fsync(f.fileno())
            stats.add(files_written=1, bytes_written=size)
            entries.append((path, hashlib.sha256(data).hexdigest()))
        except OSError as e:
            stats.error(e, path, "write")
    return bdir, entries


def verify_batch(entries, stats, phase="verify"):
    for path, digest in entries:
        try:
            with open(path, "rb") as f:
                got = hashlib.sha256(f.read()).hexdigest()
            stats.add(files_verified=1)
            if got != digest:
                stats.mismatch(path)
        except OSError as e:
            stats.error(e, path, phase)


def rewrite_rename_delete(bdir, entries, stats):
    # rewrite one file in place, rename another, then delete the whole batch
    try:
        if entries:
            path = entries[0][0]
            with open(path, "r+b") as f:
                f.seek(0)
                f.write(b"\0" * 16)
                f.flush()
                os.fsync(f.fileno())
        if len(entries) > 1:
            os.rename(entries[1][0], entries[1][0] + ".mv")
    except OSError as e:
        stats.error(e, bdir, "rewrite-rename")
    try:
        shutil.rmtree(bdir)
    except OSError as e:
        stats.error(e, bdir, "delete")


def worker(tid, args, deadline, stats, root):
    batch = 0
    live = []  # (bdir, entries), oldest first
    while time.time() < deadline:
        try:
            free = shutil.disk_usage(root).free
        except OSError as e:
            stats.error(e, root, "disk_usage")
            free = 0
        grow = free > args.min_free_gib * GIB and len(live) < args.max_live_batches
        if grow:
            res = write_batch(root, tid, batch, args.files_per_batch, stats)
            batch += 1
            if res:
                bdir, entries = res
                verify_batch(entries, stats)
                live.append(res)
        if live and (not grow or len(live) >= args.max_live_batches // 2):
            bdir, entries = live.pop(0)
            verify_batch(entries, stats, "reverify")
            rewrite_rename_delete(bdir, entries, stats)
        elif not grow and not live:
            time.sleep(1)
    for bdir, entries in live:
        verify_batch(entries, stats, "final-verify")
        try:
            shutil.rmtree(bdir)
        except OSError as e:
            stats.error(e, bdir, "cleanup")


def snapshot(stats, elapsed, root):
    try:
        free = shutil.disk_usage(root).free
    except OSError:
        free = -1
    return {
        "elapsed_s": int(elapsed),
        "files_written": stats.files_written,
        "bytes_written": stats.bytes_written,
        "files_verified": stats.files_verified,
        "verify_mismatches": stats.verify_mismatches,
        "os_errors": stats.os_errors,
        "free_bytes": free,
        "distinct_errors": stats.errors,
        "mismatch_samples": stats.mismatch_samples,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--minutes", type=float, default=20)
    ap.add_argument("--seconds", type=float, default=None, help="overrides --minutes")
    ap.add_argument("--threads", type=int, default=8)
    ap.add_argument("--files-per-batch", type=int, default=500)
    ap.add_argument("--min-free-gib", type=float, default=40)
    ap.add_argument("--max-footprint-gib", type=float, default=45)
    ap.add_argument("--report", required=True)
    args = ap.parse_args()

    duration = args.seconds if args.seconds is not None else args.minutes * 60
    avg_batch_bytes = args.files_per_batch * 34 * 1024
    per_thread = args.max_footprint_gib * GIB / max(args.threads, 1)
    args.max_live_batches = max(2, int(per_thread / avg_batch_bytes))

    os.makedirs(args.dir, exist_ok=True)
    os.makedirs(os.path.dirname(os.path.abspath(args.report)), exist_ok=True)
    stats = Stats()
    start = time.time()
    deadline = start + duration
    threads = [
        threading.Thread(target=worker, args=(i, args, deadline, stats, args.dir), daemon=True)
        for i in range(args.threads)
    ]
    for t in threads:
        t.start()
    next_tick = start + 60
    while any(t.is_alive() for t in threads):
        time.sleep(0.5)
        if time.time() >= next_tick:
            s = snapshot(stats, time.time() - start, args.dir)
            print("[%ds] files=%d bytes=%.1fGiB mismatches=%d oserrors=%d free=%.1fGiB" % (
                s["elapsed_s"], s["files_written"], s["bytes_written"] / GIB,
                s["verify_mismatches"], s["os_errors"], s["free_bytes"] / GIB), flush=True)
            next_tick += 60
    for t in threads:
        t.join()
    report = snapshot(stats, time.time() - start, args.dir)
    with open(args.report, "w") as f:
        json.dump(report, f, indent=2)
    print("done: " + json.dumps({k: v for k, v in report.items() if not isinstance(v, list)}), flush=True)
    shutil.rmtree(args.dir, ignore_errors=True)


if __name__ == "__main__":
    main()
