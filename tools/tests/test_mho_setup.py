# Copyright (C) 2026 dekajoule
# SPDX-License-Identifier: GPL-2.0-or-later

"""Offline tests of tools/mho-setup.py: which recordings go to the scope under which names. No scope."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("oos_mho_setup", ROOT / "tools/mho-setup.py")
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class Install(unittest.TestCase):
    # Break it catches: an older recording sent instead of the newest, a recording under another name than OOS reads,
    # the optional front-end set-up taken as required.
    def test_the_newest_recording_of_each_kind_under_the_name_oos_reads(self):
        with tempfile.TemporaryDirectory() as d:
            for i, name in enumerate(["mho-snapshot-MHO98-1.txt", "mho-snapshot-MHO98-2.txt",
                                      "mho-capture-MHO98-1.txt", "mho-settings-MHO98-1.txt", "notes.txt"]):
                p = os.path.join(d, name)
                Path(p).write_text("x")
                os.utime(p, (1000 + i, 1000 + i))
            files, missing = setup.plan_install(d)
            self.assertEqual(missing, [])
            self.assertEqual(sorted((os.path.basename(f), n) for f, n in files), [
                ("mho-capture-MHO98-1.txt", "mho-capture.txt"),
                ("mho-settings-MHO98-1.txt", "mho-settings.txt"),
                ("mho-snapshot-MHO98-2.txt", "mho-snapshot.txt"),
            ])
            os.remove(os.path.join(d, "mho-settings-MHO98-1.txt"))
            self.assertEqual(setup.plan_install(d)[1], ["mho-settings-*.txt"])


if __name__ == "__main__":
    unittest.main()
