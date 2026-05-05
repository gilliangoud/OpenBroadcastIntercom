import datetime as dt
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import release_version


class ReleaseVersionTests(unittest.TestCase):
    def test_no_previous_tag_starts_month_counter(self):
        self.assertEqual(
            release_version.next_calver([], dt.date(2026, 5, 5)),
            "2026.5.1",
        )

    def test_same_month_tag_increments_counter(self):
        self.assertEqual(
            release_version.next_calver(
                ["v2026.5.1", "v2026.5.7", "v2026.4.99"],
                dt.date(2026, 5, 5),
            ),
            "2026.5.8",
        )

    def test_new_month_resets_counter(self):
        self.assertEqual(
            release_version.next_calver(["v2026.5.7"], dt.date(2026, 6, 1)),
            "2026.6.1",
        )

    def test_malformed_tags_are_ignored(self):
        self.assertEqual(
            release_version.next_calver(
                ["v2026.5.zero", "2026.5.10", "v2026.13.1", "v2026.5.2"],
                dt.date(2026, 5, 5),
            ),
            "2026.5.3",
        )

    def test_version_code_is_deterministic_android_code(self):
        self.assertEqual(release_version.version_code("2026.5.12"), 26050012)


if __name__ == "__main__":
    unittest.main()
