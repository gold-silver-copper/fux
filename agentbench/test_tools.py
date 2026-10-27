"""Focused harness properties; no servers or paid agents.

    python3 -I -m unittest discover -s agentbench -p 'test_*.py'
"""
import contextlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import drivers
import guide_check
import run
import solutions


class Tools(unittest.TestCase):
    def test_success_needs_receipt_and_no_exception_and_always_cleans_up(self):
        task = {"id": "scoring", "cat": "test", "recipe": "scoring",
                "check": {"file": "receipt.txt", "equals": "OK"}}

        class Driver:
            calls = polls = sleep_s = 0
            log = []
            stopped = False

            def start(self):
                pass

            def stop(self):
                self.stopped = True

        with tempfile.TemporaryDirectory() as scratch, contextlib.chdir(scratch), \
                patch.object(run, "SCRATCH", scratch), \
                patch.object(run, "tasks", return_value=[task]):
            for receipt, error in ((True, None), (False, None),
                                   (True, RuntimeError("driver failed after the receipt")),
                                   (True, drivers.Unsupported("test"))):
                with self.subTest(receipt=receipt, error=error):
                    driver = Driver()

                    def solve(*args):
                        if receipt:
                            Path("receipt.txt").write_text("OK")
                        if error:
                            raise error

                    with contextlib.chdir(scratch), \
                            patch.object(run, "driver_for", return_value=driver), \
                            patch.object(solutions, "scoring", solve, create=True), \
                            patch.object(run, "reap", wraps=run.reap) as reap:
                        result = run.one("fux", "scoring", 0, 0, "unused")
                        self.assertEqual(result["ok"], receipt and error is None)
                        self.assertEqual(result["error"] is None, error is None)
                        self.assertTrue(driver.stopped)
                        reap.assert_called_once()

    def test_coverage_requires_words_and_reports_missing_native_and_library_names(self):
        help_text = ("Words:\n  capture              pane -- capture\n"
                     "  signal               pane -- answer\n"
                     "  choose-workspace-for client pane -- x\n"
                     "\nThe libraries (`fux help WORD` says each):\n  quiet\n"
                     "\n\nThe server: socket\n")
        known = guide_check.words_of_fux(help_text)
        self.assertIn("choose-workspace-for", known)
        source = ('class Fux(Driver):\n'
                  '    def read(self, state):\n'
                  '        self.fux(*["capture"], "quiet", "choose-workspace-for")\n'
                  '        return state["signal"]\n')
        used = guide_check.driver_words(source, known)
        self.assertEqual(used, {"capture", "quiet", "choose-workspace-for"})
        self.assertEqual(guide_check.missing_words(used, "capture quiet choose-workspace-for"), [])
        self.assertEqual(guide_check.missing_words(used, "capture quiet"), ["choose-workspace-for"])
        self.assertEqual(guide_check.missing_words(used, "capture-client quietly"),
                         ["capture", "choose-workspace-for", "quiet"])
        for text in ("", help_text.replace("  quiet", ""),
                     help_text.replace("  ", "")):
            with self.subTest(text=text), self.assertRaises(ValueError):
                guide_check.words_of_fux(text)
        with self.assertRaises(ValueError):
            guide_check.driver_words("class Fux(Driver): pass", known)


if __name__ == "__main__":
    unittest.main()
