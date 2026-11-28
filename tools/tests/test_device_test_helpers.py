# SPDX-License-Identifier: GPL-2.0-or-later

"""Offline tests of the signal helpers of tools/device-test.py. No sockets or hardware."""
import importlib.util
import math
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("oos_device_test_helpers", ROOT / "tools/device-test.py")
device_test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(device_test)


def sine(n=200_000, mid=128, amp=100, period=1000):
    return bytearray(round(mid + amp * math.sin(2 * math.pi * k / period)) for k in range(n))


class Quantile(unittest.TestCase):
    def test_noise_peaks_do_not_move_the_levels_of_a_sine(self):
        b = sine()
        for k in range(0, len(b), 5000):
            b[k] = 255 if k % 10000 else 0
        self.assertEqual((min(b), max(b)), (0, 255))
        lo, hi = device_test.quantile(b, 0.001), device_test.quantile(b, 0.999)
        self.assertEqual((lo, hi), (28, 228))

    def test_the_ends_are_the_lowest_and_highest_values(self):
        b = bytes([3, 7, 7, 9, 200])
        self.assertEqual(device_test.quantile(b, 0.0), 3)
        self.assertEqual(device_test.quantile(b, 1.0), 200)
        self.assertEqual(device_test.quantile(b, 0.999), 200)

    def test_the_value_below_which_the_fraction_lies(self):
        b = bytes(range(100))
        self.assertEqual(device_test.quantile(b, 0.5), 50)
        self.assertEqual(device_test.quantile(b, 0.25), 25)


class GeneratorOfTheModel(unittest.TestCase):
    """Only a model with a generator gets the generator's commands: a DHO804 answers them with -241, which fails
    every test on the error check."""

    def tearDown(self):
        device_test.use_profile("dho")

    def generator(self, model):
        device_test.use_profile("dho", f"RIGOL TECHNOLOGIES,{model},X,00.01.00")
        return device_test.PROFILE["generator"]

    def test_a_model_whose_name_ends_in_s_has_one(self):
        for model in ("DHO914S", "DHO924S"):
            self.assertTrue(self.generator(model), model)

    def test_the_other_models_have_none(self):
        for model in ("DHO802", "DHO804", "DHO812", "DHO814", "DHO914", "DHO924"):
            self.assertFalse(self.generator(model), model)

    def test_without_an_answer_the_first_model_of_the_family_stands_for_it(self):
        device_test.use_profile("dho")
        self.assertTrue(device_test.PROFILE["generator"])

    def test_the_rule_of_the_family_stays_unchanged(self):
        self.generator("DHO804")
        self.assertIsInstance(device_test.profile("dho")["generator"], str)


if __name__ == "__main__":
    unittest.main()
