#!/usr/bin/env python3
"""Aggregate logical payload limits through real CLI/local/serve/direct paths.

Only the SSH process boundary uses the existing local fixture. No external
server is contacted. Run BCMR=/absolute/path/to/bcmr python3 this-file.
"""
import os
import subprocess
import time
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires the POSIX SSH fixture")
class BandwidthTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok

    RATE = 256 * 1024
    PAYLOAD = 512 * 1024

    def payload(self, base, name, size=PAYLOAD, compressible=False):
        path = base / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"z" * size if compressible else os.urandom(size))
        return path

    def assert_limited(self, *args, payload=PAYLOAD, maximum=None):
        started = time.monotonic()
        result = self.run_bcmr("copy", "--bandwidth-limit=256KiB/s", *args)
        elapsed = time.monotonic() - started
        self.assert_ok(result)
        expected = payload / self.RATE
        print(f"rate receipt: {args!s}: payload={payload}, elapsed={elapsed:.3f}s", flush=True)
        self.assertGreaterEqual(elapsed, expected - 0.12,
                                "aggregate payload exceeded the invocation cap")
        if maximum is not None:
            self.assertLess(elapsed, maximum,
                            "payload appears to have been charged more than once")
        return result

    # Controls prove the real local, legacy, serve and AEAD/direct fixtures work.
    def test_fixture_routes_transfer_without_a_limit(self):
        source = self.payload(self.local, "control", 8192)
        for route, options in (("local", []), ("legacy", []),
                               ("fixture", []), ("fixture", ["--direct=direct"])):
            with self.subTest(route=route, options=options):
                target = (self.dest if route == "local" else self.home) / (
                    route + ("-direct" if options else ""))
                argument = target if route == "local" else f"{route}:{target}"
                self.assert_ok(self.run_bcmr("copy", *options, source, argument))
                self.assertEqual(target.read_bytes(), source.read_bytes())

    # Catches automatic reflink/copy_file_range bypassing the cap.
    def test_single_local_copy_obeys_the_cap_with_default_fast_paths(self):
        source = self.payload(self.local, "single")
        target = self.dest / "single"
        self.assert_limited(source, target)
        self.assertEqual(target.read_bytes(), source.read_bytes())

    # Catches a limiter allocated separately for each local worker.
    def test_four_local_workers_share_one_cap(self):
        sources = [self.payload(self.local, f"worker-{index}", self.PAYLOAD // 4)
                   for index in range(4)]
        self.assert_limited("--jobs=4", *sources, self.dest)
        for source in sources:
            self.assertEqual((self.dest / source.name).read_bytes(), source.read_bytes())

    # Catches both resetting the limiter and counting a fan-out source just once.
    def test_fanout_destinations_share_one_cap_and_each_delivery_counts(self):
        source = self.payload(self.local, "fanout", self.PAYLOAD // 2)
        self.assert_limited(source, "--to", f"fixture:{self.home}/",
                            "--to", f"second:{self.second}/")
        for target in (self.home / source.name, self.second / source.name):
            self.assertEqual(target.read_bytes(), source.read_bytes())

    # Catches per-connection caps in the serve pool.
    def test_parallel_compressed_serve_upload_counts_logical_bytes_once(self):
        sources = [self.payload(self.local, f"compressed-{index}", self.PAYLOAD // 4, True)
                   for index in range(4)]
        self.assert_limited("-P", 4, "--compress=zstd", *sources,
                            f"fixture:{self.home}/", maximum=3.8)
        for source in sources:
            self.assertEqual((self.home / source.name).read_bytes(), source.read_bytes())

    # Catches compressed-wire accounting and charging both decoding and writing.
    def test_compressed_serve_download_counts_logical_bytes_once(self):
        source = self.payload(self.home, "download", compressible=True)
        target = self.dest / "download"
        self.assert_limited("--compress=zstd", f"fixture:{source}", target, maximum=3.8)
        self.assertEqual(target.read_bytes(), source.read_bytes())

    # Direct TCP must use the same accounting above its AEAD framing.
    def test_direct_compressed_upload_counts_logical_bytes_once(self):
        source = self.payload(self.local, "direct", compressible=True)
        target = self.home / "direct"
        self.assert_limited("--direct=direct", "--compress=zstd", source,
                            f"fixture:{target}", maximum=3.8)
        self.assertEqual(target.read_bytes(), source.read_bytes())

    def test_direct_compressed_download_counts_logical_bytes_once(self):
        source = self.payload(self.home, "direct-get", compressible=True)
        target = self.dest / "direct-get"
        self.assert_limited("--direct=direct", "--compress=zstd", f"fixture:{source}",
                            target, maximum=3.8)
        self.assertEqual(target.read_bytes(), source.read_bytes())

    def test_legacy_upload_and_download_obey_the_cap(self):
        for upload in (True, False):
            with self.subTest(upload=upload):
                source = self.payload(self.local if upload else self.home,
                                      "legacy-upload" if upload else "legacy-download")
                target = (self.home if upload else self.dest) / source.name
                src = source if upload else f"legacy:{source}"
                dst = f"legacy:{target}" if upload else target
                self.assert_limited(src, dst)
                self.assertEqual(target.read_bytes(), source.read_bytes())

    # Catches charging the validated destination prefix when seeding a stage.
    def test_local_append_charges_only_new_source_payload(self):
        source = self.payload(self.local, "append", self.PAYLOAD * 2)
        target = self.dest / "append"
        target.write_bytes(source.read_bytes()[:self.PAYLOAD])
        self.assert_limited("--append", source, target, maximum=3.8)
        self.assertEqual(target.read_bytes(), source.read_bytes())

    # A full first buffer may not be written immediately from an idle bucket.
    def test_initial_local_payload_has_no_unbounded_burst(self):
        source = self.payload(self.local, "burst", 2 * self.PAYLOAD)
        target = self.dest / "burst"
        started = time.monotonic()
        child = subprocess.Popen(
            [fixture.BCMR, "copy", "--progress=off", "--bandwidth-limit=256KiB/s",
             str(source), str(target)], cwd=self.local, env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            time.sleep(0.35)
            elapsed = time.monotonic() - started
            files = list(self.dest.glob(".bcmr.stage.*"))
            if target.exists():
                files.append(target)
            written = sum(path.stat().st_size for path in files)
            self.assertLessEqual(written, int(elapsed * self.RATE) + 65536,
                                 "first payload bypassed pacing")
            stdout, stderr = child.communicate(timeout=15)
            self.assertEqual(child.returncode, 0, stdout + stderr)
            self.assertEqual(target.read_bytes(), source.read_bytes())
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=5)

    def test_rate_units_are_accepted_and_invalid_rates_fail_before_output(self):
        source = self.payload(self.local, "unit-source", 0)
        for index, rate in enumerate(("1", "1/s", "1KiB", "1MiB/s", "1GiB",
                                      "1kB", "1MB/s", "1GB", "18446744073709551615")):
            with self.subTest(rate=rate):
                target = self.dest / f"unit-{index}"
                self.assert_ok(self.run_bcmr("copy", f"--bandwidth-limit={rate}", source, target))
                self.assertEqual(target.read_bytes(), b"")
        for index, rate in enumerate(("0", "0KiB", "-1", "1.5MiB", "1kb", "1Mbps",
                                      "1KiB/s/s", "18446744073709551616", "18446744073709551615GiB")):
            with self.subTest(rate=rate):
                target = self.dest / f"invalid-{index}"
                result = self.run_bcmr("copy", f"--bandwidth-limit={rate}", source, target)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertNotIn("unexpected argument", result.stderr)
                self.assertFalse(target.exists())

    def test_forced_reflink_with_a_cap_is_refused_before_output(self):
        source = self.payload(self.local, "forced-reflink", 8192)
        target = self.dest / "forced-reflink"
        result = self.run_bcmr("copy", "--reflink=force", "--bandwidth-limit=1MiB", source, target)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("unexpected argument", result.stderr)
        self.assertRegex(result.stderr.lower(), r"reflink.*bandwidth|bandwidth.*reflink")
        self.assertFalse(target.exists())
        self.assertEqual(list(self.dest.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
