"""Focused harness properties; no servers or paid agents.

    python3 -I -m unittest discover -s agentbench -p 'test_*.py'
"""
import contextlib
import json
from types import SimpleNamespace
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import drivers
import guide_check
import run
import solutions
import published
import report


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

    def test_grift_data_reads_back_as_records_lists_and_atoms(self):
        data = drivers.read_data(
            '((id %1:RUN) (text "a\\"b\\n\\x1b;") (exited none) (rows 24) '
            '(alive #true) (ratio 0.5) (panes ()) (args ("x" "y")))')
        self.assertEqual(data, {"id": "%1:RUN", "text": 'a"b\n\x1b', "exited": None, "rows": 24,
                                "alive": True, "ratio": 0.5, "panes": [], "args": ["x", "y"]})
        self.assertEqual(drivers.short(data["id"]), "%1")
        self.assertEqual(drivers.read_data(drivers.string('a"b\\c')), 'a"b\\c')
        for text in ("", "(1", "1 2"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                drivers.read_data(text)

    def test_first_exit_driver_waits_past_a_batch_timeout(self):
        driver = drivers.Fux.__new__(drivers.Fux)
        batches = iter([
            '((events ()) (next "0:RUN") (missed 0) (status "timeout"))',
            '((events (((event "exited") (pane %1:RUN)))) (next "1:RUN") (missed 0) '
            '(status "continuing"))',
        ])
        calls = []
        def fux(program, **kwargs):
            calls.append((program, kwargs["timeout"]))
            return SimpleNamespace(stdout=next(batches))
        driver.fux = fux
        with patch.object(drivers.time, "monotonic", side_effect=[0, 0, 5]):
            self.assertEqual(driver.wait_first_exit({"a": "%1"}, "0:RUN", timeout=15), "a")
        self.assertEqual([call[0] for call in calls], ['(data-text (next-events (all) "0:RUN" 5))'] * 2)
        self.assertEqual([call[1] for call in calls], [15, 10])

    def test_first_exit_driver_continues_from_filtered_batch_prefixes(self):
        driver = drivers.Fux.__new__(drivers.Fux)
        batches = iter([
            '((events ()) (next "256:RUN") (missed 0) (status "continuing"))',
            '((events (((event "exited") (pane %1:RUN)))) (next "257:RUN") (missed 0) '
            '(status "continuing"))',
        ])
        calls = []

        def fux(program, **kwargs):
            calls.append(program)
            return SimpleNamespace(stdout=next(batches))

        driver.fux = fux
        self.assertEqual(driver.wait_first_exit({"a": "%1"}, "0:RUN"), "a")
        self.assertEqual(calls, ['(data-text (next-events (all) "0:RUN" 5))', '(data-text (next-events (all) "256:RUN" 5))'])

    def test_published_tasks_are_scripted_fux_only_and_count_in_reports(self):
        tasks = run.tasks()
        guide = [task for task in tasks if task["cat"] == "published"]
        tools = ["fux", "tmux", "zellij", "herdr"]
        pairs = run.task_pairs(guide, tools, "scripted")
        self.assertEqual([(tool, task["id"]) for tool, task in pairs],
                         [("fux", task["id"]) for task in guide])
        self.assertEqual(run.task_pairs(guide, tools, "agents"), [])
        # An ordinary comparative task is scheduled even if its driver may
        # report unsupported: applicability does not turn that into a skip.
        ordinary = {"id": "ordinary"}
        self.assertEqual(len(run.task_pairs([ordinary], tools, "scripted")), 4)
        results = [{"tool": tool, "cat": task["cat"], "task": task["id"],
                    "ok": True, "calls": 1, "sleep_s": 0} for tool, task in pairs]
        table = report.scripted_table(results, "guide")
        self.assertIn("| published | 4/4", table)
        self.assertIn("| **all** | **4/4**", table)
        results[0]["ok"] = False
        self.assertIn("| **all** | **3/4**", report.scripted_table(results, "guide"))

    def test_published_first_exit_uses_an_independent_prelaunch_cursor(self):
        receipt = '("9:RUN" ("%1:RUN" "%2:RUN") ((event "exited") (pane %1:RUN)))'
        with tempfile.TemporaryDirectory() as work:
            calls = []

            def cursor():
                calls.append("cursor")
                return "0:RUN"

            def script(*args, **kwargs):
                calls.append("recipe")
                return SimpleNamespace(stdout=receipt)

            driver = SimpleNamespace(name="fux", work=work, bin="fux",
                                     spawn=lambda *a: "%0", kill=lambda *a: None,
                                     events_cursor=cursor, run=script)
            for recorded in ("%1", "%2"):
                with self.subTest(recorded=recorded):
                    calls.clear()

                    def replay(panes, since):
                        self.assertEqual(since, "0:RUN")
                        return recorded

                    driver.wait_first_exit = replay
                    if recorded == "%1":
                        self.assertEqual(published.first_exit(driver, {}, ""), "recorded")
                    else:
                        with self.assertRaisesRegex(RuntimeError, "differs from"):
                            published.first_exit(driver, {}, "")
                    self.assertEqual(calls, ["cursor", "recipe"])

    def test_title_lookup_uses_one_topology_observation(self):
        driver = drivers.Fux.__new__(drivers.Fux)
        reply = SimpleNamespace(stdout='((panes (((id %1:RUN) (title "other")) '
                                       '((id %2:RUN) (title "ledger")))))')
        with patch.object(driver, "fux", return_value=reply) as ask:
            self.assertEqual(driver.find_title("ledger"), "%2")
            ask.assert_called_once()

    def test_coverage_requires_words_and_reports_missing_native_and_library_names(self):
        dictionary = ("capture fux\nsignal fux\nchoose-workspace-for fux\n"
                      "quiet library\nfirst core\n")
        known = guide_check.words_of_fux(dictionary)
        self.assertEqual(known, {"capture", "signal", "choose-workspace-for", "quiet"})
        source = ('class Fux(Driver):\n'
                  '    def read(self, state):\n'
                  '        self.fux("(capture (quiet (first (choose-workspace-for))))")\n'
                  '        return state["(signal"]\n')
        used = guide_check.driver_words(source, known)
        self.assertEqual(used, {"capture", "quiet", "choose-workspace-for"})
        self.assertEqual(guide_check.missing_words(used, "capture quiet choose-workspace-for"), [])
        self.assertEqual(guide_check.missing_words(used, "capture quiet"), ["choose-workspace-for"])
        self.assertEqual(guide_check.missing_words(used, "captured quietly"),
                         ["capture", "choose-workspace-for", "quiet"])
        with self.assertRaises(ValueError):
            guide_check.words_of_fux("first core\n")
        with self.assertRaises(ValueError):
            guide_check.driver_words("class Fux(Driver): pass", known)


if __name__ == "__main__":
    unittest.main()
