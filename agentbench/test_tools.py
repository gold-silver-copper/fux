"""Focused harness properties; no servers or paid agents.

    python3 -I -m unittest discover -s agentbench -p 'test_*.py'
"""
import contextlib
import io
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

    def test_published_journal_recipe_saves_cursor_before_launch_and_refuses_loss(self):
        namespace = {"__name__": "test_recipe"}
        exec(published.block("**First recorded exit across jobs:**", "python"), namespace)
        for event in ({"event": "exited", "pane": "%1"},
                      {"event": "exited", "pane": "%2"},
                      {"event": "exited", "pane": "%3"},
                      {"event": "missed", "count": 1}):
            calls = []
            subscriber = SimpleNamespace(stdout=io.StringIO(json.dumps(event) + "\n"),
                                         terminate=lambda: calls.append("terminate"),
                                         wait=lambda **kwargs: calls.append("wait"))

            def fux(*words, stream=False):
                calls.append(words)
                if words == ("ls", "json"):
                    return '{"events":"0:RUN"}'
                if words[-1] == "run":
                    return f"%{sum(isinstance(c, tuple) and c[-1] == 'run' for c in calls)}:0:RUN"
                if stream:
                    return subscriber
                return ""

            with self.subTest(event=event):
                if event["event"] == "missed":
                    with self.assertRaisesRegex(RuntimeError, "first exit is unknown"):
                        namespace["first_exit"](fux)
                else:
                    receipt = namespace["first_exit"](fux)
                    self.assertEqual(receipt["first"], event)
                    self.assertEqual(receipt["cursor"], "0:RUN")
                    self.assertEqual(receipt["panes"], {"%1": "a", "%2": "b", "%3": "c"})
                self.assertEqual(calls[0], ("ls", "json"))
                self.assertEqual(calls[4], ("all", "str", "0:RUN", "from", "events"))
                self.assertIn("terminate", calls)
                self.assertIn("wait", calls)
                self.assertTrue(subscriber.stdout.closed)
                self.assertEqual(calls[-3:], [("str", f"%{n}", "pane", "kill") for n in (1, 2, 3)])

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
        receipt = {"cursor": "9:RUN", "panes": {"%1": "a", "%2": "b"},
                   "first": {"event": "exited", "pane": "%1"}}
        with tempfile.TemporaryDirectory() as work:
            (Path(work) / "fux-calls").write_text(".\n" * 8)
            calls = []

            def cursor():
                calls.append("cursor")
                return "0:RUN"

            def script(*args, **kwargs):
                calls.append("recipe")
                return SimpleNamespace(stdout=json.dumps(receipt))

            driver = SimpleNamespace(name="fux", work=work, calls=0,
                                     spawn=lambda *a: "%0", kill=lambda *a: None,
                                     events_cursor=cursor, run=script)
            for recorded in ("a", "b"):
                with self.subTest(recorded=recorded):
                    calls.clear()

                    def replay(panes, since):
                        self.assertEqual(since, "0:RUN")
                        return recorded

                    driver.wait_first_exit = replay
                    if recorded == "a":
                        self.assertEqual(published.first_exit(driver, {}, ""), "recorded")
                    else:
                        with self.assertRaisesRegex(RuntimeError, "differs from"):
                            published.first_exit(driver, {}, "")
                    self.assertEqual(calls, ["cursor", "recipe"])

    def test_title_lookup_uses_one_topology_observation(self):
        driver = drivers.Fux.__new__(drivers.Fux)
        reply = SimpleNamespace(stdout=json.dumps({"panes": [
            {"id": "%1", "title": "other"}, {"id": "%2", "title": "ledger"}]}))
        with patch.object(driver, "fux", return_value=reply) as ask:
            self.assertEqual(driver.find_title("ledger"), "%2")
            ask.assert_called_once()

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
        self.assertEqual(guide_check.missing_words(used, "captured quietly"),
                         ["capture", "choose-workspace-for", "quiet"])
        for text in ("", help_text.replace("  quiet", ""),
                     help_text.replace("  ", "")):
            with self.subTest(text=text), self.assertRaises(ValueError):
                guide_check.words_of_fux(text)
        with self.assertRaises(ValueError):
            guide_check.driver_words("class Fux(Driver): pass", known)


if __name__ == "__main__":
    unittest.main()
