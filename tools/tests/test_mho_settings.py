# SPDX-License-Identifier: GPL-2.0-or-later

"""Offline tests of tools/mho-settings.py: the steps it takes and the part it writes for OOS. No adb, no scope."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("oos_mho_settings", ROOT / "tools/mho-settings.py")
ms = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ms)


class Steps(unittest.TestCase):
    def test_every_channel_combination_and_one_channel_always_on(self):
        self.assertEqual(len(ms.COMBOS), 15)
        cmds = ms.channels_commands((2, 4))
        self.assertEqual(cmds[:2], [":CHANnel2:DISPlay ON", ":CHANnel4:DISPlay ON"])
        self.assertEqual(cmds[2:], [":CHANnel1:DISPlay OFF", ":CHANnel3:DISPlay OFF"])

    def test_the_plan_reads_the_adcs_where_the_sampling_changes_only(self):
        steps = ms.plan()
        names = [n for n, _, _ in steps]
        self.assertEqual(len(steps), 1 + 2 * (1 + len(ms.TIMEBASES)) + 1 + 15 + 1 + 4 * (2 + 13))
        adc = [n for n, _, r in steps if r]
        self.assertTrue(all("tdiv=" in n or "chans=" in n for n in adc))
        self.assertNotIn("CH3 scale=0.1", adc)
        self.assertIn("CH3 scale=0.1", names)
        self.assertLess(len(ms.plan(quick=True)), len(steps))

    def test_a_setting_of_the_part_for_oos(self):
        state = ms.state_line(["1", "0", "0", "0", "1.0E0", "1.0E0", "1.0E0", "1.0E0", "2.0E-4", "5.0E6", "AUTO"])
        self.assertEqual(state, "state on1=1 on2=0 on3=0 on4=0 scale1=1.0E0 scale2=1.0E0 scale3=1.0E0 scale4=1.0E0 "
                                "tdiv=2.0E-4 srate=5.0E6 mdepth=AUTO")
        lines = ms.setting_lines("CH1 tdiv=1e-6", [":TIMebase:MAIN:SCALe 1e-6"], state, {0x10d8: 5},
                                 [(0, {0x54: 0x1803})], [("CH1", 0x07, 0xc440)], [("a149", 0)])
        self.assertEqual(lines, ["setting CH1_tdiv=1e-6", "scpi :TIMebase:MAIN:SCALe 1e-6", state,
                                 "fpga 0x10d8 0x00000005", "adc 0 0x54 0x1803", "afe 1 0x07 0xc440", "select a149 0",
                                 "endsetting"])


if __name__ == "__main__":
    unittest.main()
