import unittest

from main import divide


class DivisionTests(unittest.TestCase):
    def test_nonzero(self):
        self.assertEqual(divide(6, 2), 3)

    def test_zero(self):
        self.assertIsNone(divide(6, 0))
